using System.Net;
using System.Net.Sockets;
using Mvf.Graph.Execution;

namespace Mvf.Egress;

/// <summary>
/// An <see cref="IEgressSink"/> that fans <b>state</b> records out over UDP multicast — a connectionless,
/// subscriber-less LAN broadcast. Ideal for lightweight state telemetry to many listeners at once. It is
/// <b>state-only</b>: a frame payload exceeds a datagram's MTU, so <see cref="WantsFrames"/> is always false
/// and a frame publish degrades to its state record. Each record is one datagram carrying the record body
/// (the length prefix is dropped — the datagram boundary frames it), so a listener decodes it directly.
/// </summary>
public sealed class UdpEgressSink : IEgressSink
{
    /// <summary>The multicast group the state stream is broadcast on (distinct from the discovery group).</summary>
    public const string DataGroup = "239.255.7.72";

    private const int MaxDatagram = 65000;

    private readonly EgressRecordQueue _queue = new();
    private readonly UdpClient _udp;
    private readonly IPEndPoint _endpoint;
    private readonly CancellationTokenSource _cts = new();
    private readonly Task _drainLoop;

    public UdpEgressSink(int port)
    {
        Port = port;
        _endpoint = new IPEndPoint(IPAddress.Parse(DataGroup), port);
        _udp = new UdpClient(AddressFamily.InterNetwork) { Ttl = 1 };
        _drainLoop = Task.Run(() => _queue.DrainAsync(BroadcastAsync, _cts.Token));
    }

    public int Port { get; }

    public void PublishCycle(PipelineExecutionProgress progress) =>
        _queue.Enqueue(EgressRecordQueue.Queued.ForCycle(progress));

    public void PublishNodeTransition(NodeExecutionEvent nodeEvent) =>
        _queue.Enqueue(EgressRecordQueue.Queued.ForNode(nodeEvent));

    // UDP is state-only; a frame publish (which never happens while WantsFrames is false) degrades to state.
    public void PublishNodeTransitionFrame(
        NodeExecutionEvent nodeEvent, ReadOnlyMemory<byte> payloadDescriptor, ReadOnlyMemory<byte> payload) =>
        _queue.Enqueue(EgressRecordQueue.Queued.ForNode(nodeEvent));

    public bool WantsFrames => false;

    public EgressSinkStats Stats => new(_queue.Published, _queue.Dropped, 0);

    private async Task BroadcastAsync(byte[] frame)
    {
        var body = frame.AsMemory(4); // drop the 4-byte length prefix; the datagram is the boundary
        if (body.Length > MaxDatagram)
        {
            return; // a state record should never approach the MTU; skip rather than fragment
        }

        try
        {
            await _udp.SendAsync(body, _endpoint, _cts.Token).ConfigureAwait(false);
        }
        catch
        {
            // best effort — a missing NIC or firewall must never fault the run
        }
    }

    public void Dispose()
    {
        _cts.Cancel();
        _queue.Complete();

        try
        {
            _drainLoop.Wait(TimeSpan.FromSeconds(2));
        }
        catch
        {
            // shutting down
        }

        _udp.Dispose();
        _cts.Dispose();
    }
}
