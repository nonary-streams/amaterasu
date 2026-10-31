// Drives a private Chromium debugger and the test-only loopback peer. No user
// browser profile, installed service, remote listener, or input device is used.
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { createInterface } from "node:readline";

const [debuggerAddress, peerExecutable, mode = "media"] = process.argv.slice(2);
if (!["media", "session"].includes(mode)) throw new Error("Invalid qualification mode");
const address = new URL(debuggerAddress);
if (address.protocol !== "ws:" || address.hostname !== "127.0.0.1" ||
    !/^\/devtools\/browser\/[a-f0-9-]+$/.test(address.pathname)) throw new Error("Invalid debugger address");
const server = createServer((_request, response) => response.end("<!doctype html><html><body></body></html>"));
await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
const peer = spawn(peerExecutable, [], { stdio:["pipe", "pipe", "pipe"] });
let peerError = "";
peer.stderr.on("data", (data) => { peerError += data.toString(); });
const peerExit = new Promise((resolve) => peer.once("exit", resolve));
const socket = new WebSocket(address);
let next = 0;
const pending = new Map();
socket.addEventListener("message", ({data}) => {
  const message = JSON.parse(data);
  const request = pending.get(message.id);
  if (!request) return;
  pending.delete(message.id);
  clearTimeout(request.timeout);
  if (message.error) request.reject(new Error("Browser debugger command failed"));
  else request.resolve(message.result);
});
function command(method, params = {}, sessionId) {
  const id = ++next;
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => { pending.delete(id); reject(new Error(`Browser debugger timed out: ${method}`)); }, 15_000);
    pending.set(id, { resolve, reject, timeout });
    socket.send(JSON.stringify({id,method,params,...(sessionId ? {sessionId} : {})}));
  });
}
function check(value, reason) { if (!value) throw new Error(reason); }
try {
  await new Promise((resolve, reject) => { socket.addEventListener("open", resolve, {once:true}); socket.addEventListener("error", reject, {once:true}); });
  const {targetId} = await command("Target.createTarget", {url:"about:blank"});
  const {sessionId} = await command("Target.attachToTarget", {targetId,flatten:true});
  await command("Page.enable", {}, sessionId);
  await command("Runtime.enable", {}, sessionId);
  await command("Page.navigate", {url:`http://127.0.0.1:${server.address().port}/`}, sessionId);
  async function evaluate(expression) {
    const reply = await command("Runtime.evaluate", {expression,awaitPromise:true,returnByValue:true}, sessionId);
    if (reply.exceptionDetails) throw new Error(`Browser evaluation failed: ${reply.exceptionDetails.exception?.description ?? reply.exceptionDetails.text}`);
    return reply.result.value;
  }
  const deadline = Date.now() + 10_000;
  while (!(await evaluate("document.readyState === 'complete' && location.hostname === '127.0.0.1'"))) {
    check(Date.now() < deadline, "Browser page did not load");
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  const offer = await evaluate(`(async()=>{
    const peer=window.peer=new RTCPeerConnection({iceServers:[]});
    const video=peer.addTransceiver('video',{direction:'recvonly'});
    peer.addTransceiver('audio',{direction:'recvonly'});
    video.setCodecPreferences(RTCRtpReceiver.getCapabilities('video').codecs.filter(codec=>codec.mimeType.toLowerCase()==='video/h264'));
    const channel=window.channel=peer.createDataChannel('input');
    channel.onopen=()=>channel.send('probe-input');
    peer.ontrack=event=>{const element=document.createElement(event.track.kind);element.autoplay=true;element.muted=true;
      element.srcObject=new MediaStream([event.track]);document.body.append(element);element.play().catch(()=>{});};
    await peer.setLocalDescription(await peer.createOffer());
    await new Promise((resolve,reject)=>{if(peer.iceGatheringState==='complete')return resolve();
      const timer=setTimeout(()=>reject(Error('ICE gathering timed out')),5000);
      peer.addEventListener('icegatheringstatechange',()=>{if(peer.iceGatheringState==='complete'){clearTimeout(timer);resolve();}});});
    return peer.localDescription.toJSON();
  })()`);
  const answer = new Promise((resolve, reject) => {
    const lines = createInterface({input:peer.stdout});
    lines.once("line", (line) => { lines.close(); try { resolve(JSON.parse(line)); } catch { reject(new Error("Invalid peer answer")); } });
    peer.once("exit", (code) => reject(new Error(`Loopback peer exited early (${code}): ${peerError}`)));
  });
  peer.stdin.write(JSON.stringify(offer) + "\n");
  const response = await answer;
  await evaluate(`peer.setRemoteDescription(${JSON.stringify(response)})`);
  let statistics;
  const mediaDeadline = Date.now() + 12_000;
  while (Date.now() < mediaDeadline) {
    statistics = await evaluate(`(async()=>{const stats=await peer.getStats();const inbound=[...stats.values()].filter(item=>item.type==='inbound-rtp');
      return {state:peer.connectionState,channel:channel.readyState,video:inbound.find(item=>item.kind==='video')?.framesDecoded ?? 0,
        audio:inbound.find(item=>item.kind==='audio')?.packetsReceived ?? 0};})()`);
    if (mode === "session" ? peerError.includes("session closed") :
        statistics.video >= 60 && statistics.audio >= 60 && statistics.channel === "open" && peerError.includes("input received")) break;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  check(mode === "session" ? peerError.includes("input received") && peerError.includes("session closed") :
    statistics?.video >= 60 && statistics?.audio >= 60 && statistics?.channel === "open" && peerError.includes("input received"),
    `WebRTC ${mode} did not progress: ${JSON.stringify(statistics)} ${peerError}`);
  await command("Target.closeTarget", {targetId});
  console.log(mode === "session" ? "Passed: managed loopback session received browser data and closed." :
    `Passed: loopback Chromium decoded ${statistics.video} H.264 frames, received ${statistics.audio} Opus packets, and sent a data-channel message.`);
} finally {
  socket.close();
  server.close();
  if (peer.exitCode === null && peer.signalCode === null) peer.kill();
  await peerExit;
}
