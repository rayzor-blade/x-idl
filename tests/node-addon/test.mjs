import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { Worker } from 'node:worker_threads';
import { bind, Mode, operations } from './generated.ts';

const require = createRequire(import.meta.url);
const nativePath = require.resolve('./test.node');
const native = require(nativePath);
const { Device, Other } = bind(native);
const config = {
  title: 'hello',
  number: 9007199254740993n,
  mode: Mode.Loud,
  optional: 'extra',
  optional_mode: Mode.Quiet,
  choices: [Mode.Quiet,Mode.Loud],
  settings: new Map([['gain',2]]),
  nested: { gain: .5 },
  children: [{ gain: .25 }],
  bytes: new Uint8Array([4,5]),
  payload: { kind: 'Nested', value: { gain: .75 } },
};
const device = Device.open(config);
assert.equal(device.text(),'native ✓');
assert.equal(device.echo(9007199254740993n),9007199254740993n);
assert.equal(device.mode(null),-1);
assert.equal(device.mode(undefined),-1);
assert.equal(device.mode(Mode.Loud),9);
assert.deepEqual(device.bytes(),new Uint8Array([7,8,9]));
assert.deepEqual(device.event(),{ kind:'Data',mode:Mode.Quiet,bytes:new Uint8Array([1,2,3]),detail:{kind:'Text',text:'event',stamp:9007199254740993n}});
const target = new Uint8Array([0,0,0]);
device.write(target);
assert.deepEqual(target,new Uint8Array([8,7,6]));
const later = await device.later(false);
assert.equal(later.text(),'native ✓');
await assert.rejects(device.later(true),/async rejected/);
assert.equal(await device.done(),undefined);
assert.throws(()=>device.echo(1n<<63n),/out of range/);
assert.throws(()=>device.echo(1),/BigInt|bigint/i);
assert.throws(()=>device.mode(7),/unknown enum/);
assert.throws(()=>device.fail(),/backend validation/);
assert.throws(()=>device.panic(),/backend panic/);
assert.equal(device.text(),'native ✓'); // Failure must not poison a later call.
assert.throws(()=>native.call(operations['Device.text'],[{}]),/external|wrapped object/i);
assert.throws(()=>native.call(operations['Device.text'],[2]),/external|wrapped object/i);
// Capture opaque native payloads through a transport wrapper to exercise type validation.
let otherPayload;
const capture = bind({call(name,args){ const result=native.call(name,args); otherPayload=result; return result; }});
capture.Other.open();
assert.throws(()=>native.call(operations['Device.text'],[otherPayload]),/wrapped object/i);
assert.throws(()=>native.call(operations['Device.text'],[]),/number of arguments/);
assert.throws(()=>native.call(4294967294,[]),/unknown native method/);
assert.throws(()=>native.call(operations['Device.open'],[{...config,settings:[['gain',2]],get title(){ device.text(); return 'hello'; }}]),/reentrant/);
const detached=new Uint8Array([4,5]);
structuredClone(detached.buffer,{transfer:[detached.buffer]});
assert.throws(()=>Device.open({...config,bytes:detached}),/detached/);
assert.throws(()=>Device.open({...config,bytes:new Uint8Array(new SharedArrayBuffer(2))}),/shared/);
await new Promise((resolve,reject)=>{
  const worker=new Worker(`const {parentPort}=require('node:worker_threads');try{require(${JSON.stringify(nativePath)}).call(0,[]);parentPort.postMessage('unexpected');}catch(e){parentPort.postMessage(e.message);}`,{eval:true});
  worker.once('message',message=>{try{assert.match(message,/owning thread/);resolve();}catch(e){reject(e);}});
  worker.once('error',reject);
});
assert.throws(()=>bind({call(){return 'stale';}}),/schema mismatch/);
console.log('Node binding integration passed');
