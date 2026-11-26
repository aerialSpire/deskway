const TIMEOUT_MS = 5000;

export async function probeRelay(turn, stunURL, Peer) {
  let a, b, timer;
  let allocated = false;
  try {
    a = new Peer({ iceTransportPolicy: 'relay', iceServers: [turn] });
    b = new Peer({ iceServers: [{ urls: stunURL }] });
    const channel = a.createDataChannel('relay-check');
    const opened = new Promise((resolve) => channel.addEventListener('open', () => resolve(true)));

    a.onicecandidate = ({ candidate }) => {
      if (!candidate) return;
      allocated = true;
      b.addIceCandidate(candidate).catch(() => {});
    };
    b.onicecandidate = ({ candidate }) => {
      if (candidate) a.addIceCandidate(candidate).catch(() => {});
    };

    const negotiate = async () => {
      // Set each peer's remote description before the other peer's local description starts ICE gathering.
      const offer = await a.createOffer();
      await b.setRemoteDescription(offer);
      const answer = await b.createAnswer();
      await a.setLocalDescription(offer);
      await a.setRemoteDescription(answer);
      await b.setLocalDescription(answer);
      return opened;
    };
    const deadline = new Promise((resolve) => { timer = setTimeout(resolve, TIMEOUT_MS); });
    if (await Promise.race([negotiate(), deadline])) return 'connected';
  } catch {
    // A failed negotiation uses the same allocation result as the deadline.
  } finally {
    clearTimeout(timer);
    a?.close();
    b?.close();
  }
  return allocated ? 'allocated' : 'unreachable';
}
