using System.Net;
using System.Net.Sockets;
using Mvf.Graph.Execution;

namespace Mvf.Egress;

/// <summary>
/// An <see cref="IEgressSink"/> that serves the framed record stream over TCP. Subscribers (native / Node
/// consumers) connect to the port and receive every record. Publish is non-blocking (see
/// <see cref="EgressRecordQueue"/>): a slow subscriber slows the drain, fills the ring, and causes drops,
/// but never stalls the graph.
/// </summary>
public sealed class TcpServerEgressSink : IEgressSink
{
    private readonly EgressRecordQueue _queue = new();
    private readonly EgressStreams _streams;
    private readonly TcpListener _listener;
    private readonly CancellationTokenSource _cts = new();
    private readonly List<TcpClient> _clients = [];
    private readonly object _clientsLock = new();
    private readonly Task _acceptLoop;
    private readonly Task _drainLoop;

    // The encoded topology record, kept outside the ring: it must not compete for ring space with the live
    // stream (where dropping is correct), and every subscriber that attaches mid-run still needs it.
    private volatile byte[]? _topologyFrame;

    /// <param name="bindAddress">
    /// Which interface to serve on. Defaults to loopback: the stream carries a plant's live production
    /// state and has no authentication, so reaching the network must be a decision someone typed, not
    /// something a default did quietly. Pass <see cref="IPAddress.Any"/> to let other machines attach.
    /// </param>
    public TcpServerEgressSink(int port, EgressStreams streams = EgressStreams.State, IPAddress? bindAddress = null)
    {
        _streams = streams;
        _listener = new TcpListener(bindAddress ?? IPAddress.Loopback, port);
        _listener.Start();
        Port = ((IPEndPoint)_listener.LocalEndpoint).Port;

        _acceptLoop = Task.Run(AcceptLoopAsync);
        _drainLoop = Task.Run(() => _queue.DrainAsync(BroadcastAsync, _cts.Token));
    }

    /// <summary>The bound port (useful when constructed with port 0 for an ephemeral port).</summary>
    public int Port { get; }

    /// <summary>
    /// Keeps the topology twice over, because the two audiences need it differently: subscribers already
    /// attached when the run starts get it through the ring, in order with everything else, while a
    /// subscriber that attaches later gets the retained copy as a preamble. Doing only the second would
    /// leave a viewer that was waiting before the run began without a graph. The retained copy carries
    /// seq 0 — it is a replay, not a position in the live sequence.
    /// </summary>
    public void PublishTopology(EgressTopology topology)
    {
        _topologyFrame = EgressWire.EncodeTopology(topology, topology.RunId, seq: 0);
        _queue.Enqueue(EgressRecordQueue.Queued.ForTopology(topology));
    }

    public void PublishCycle(PipelineExecutionProgress progress) =>
        _queue.Enqueue(EgressRecordQueue.Queued.ForCycle(progress));

    public void PublishNodeTransition(NodeExecutionEvent nodeEvent) =>
        _queue.Enqueue(EgressRecordQueue.Queued.ForNode(nodeEvent));

    public void PublishNodeTransitionFrame(
        NodeExecutionEvent nodeEvent, ReadOnlyMemory<byte> payloadDescriptor, ReadOnlyMemory<byte> payload) =>
        _queue.Enqueue(EgressRecordQueue.Queued.ForNodeFrame(nodeEvent, payloadDescriptor, payload));

    public bool WantsFrames => (_streams & EgressStreams.Frame) != 0 && SubscriberCount > 0;

    public EgressSinkStats Stats => new(_queue.Published, _queue.Dropped, SubscriberCount);

    private int SubscriberCount
    {
        get
        {
            lock (_clientsLock)
            {
                return _clients.Count;
            }
        }
    }

    private async Task AcceptLoopAsync()
    {
        try
        {
            while (!_cts.IsCancellationRequested)
            {
                var client = await _listener.AcceptTcpClientAsync(_cts.Token).ConfigureAwait(false);
                client.NoDelay = true;

                // Write the topology preamble *before* registering the client, so this write can never
                // interleave with the drain loop's broadcast and split a frame down the middle. Records
                // published during the write are missed by this subscriber — acceptable on a best-effort
                // stream, and the next cycle repaints its state anyway.
                var preamble = _topologyFrame;
                if (preamble is not null)
                {
                    try
                    {
                        await client.GetStream().WriteAsync(preamble, _cts.Token).ConfigureAwait(false);
                    }
                    catch (Exception ex) when (ex is IOException or SocketException or ObjectDisposedException)
                    {
                        client.Dispose();
                        continue;
                    }
                }

                lock (_clientsLock)
                {
                    _clients.Add(client);
                }
            }
        }
        catch (OperationCanceledException) { }
        catch (ObjectDisposedException) { }
        catch (SocketException) { }
    }

    private async Task BroadcastAsync(byte[] frame)
    {
        List<TcpClient> snapshot;
        lock (_clientsLock)
        {
            if (_clients.Count == 0)
            {
                return;
            }

            snapshot = [.. _clients];
        }

        foreach (var client in snapshot)
        {
            try
            {
                await client.GetStream().WriteAsync(frame, _cts.Token).ConfigureAwait(false);
            }
            catch
            {
                RemoveClient(client);
            }
        }
    }

    private void RemoveClient(TcpClient client)
    {
        lock (_clientsLock)
        {
            _clients.Remove(client);
        }

        try
        {
            client.Dispose();
        }
        catch
        {
            // best effort
        }
    }

    public void Dispose()
    {
        _cts.Cancel();
        try
        {
            _listener.Stop();
        }
        catch
        {
            // ignore
        }

        _queue.Complete();

        try
        {
            Task.WhenAll(_acceptLoop, _drainLoop).Wait(TimeSpan.FromSeconds(2));
        }
        catch
        {
            // shutting down — a loop faulting on a cancelled socket is expected
        }

        lock (_clientsLock)
        {
            foreach (var client in _clients)
            {
                try
                {
                    client.Dispose();
                }
                catch
                {
                    // best effort
                }
            }

            _clients.Clear();
        }

        _cts.Dispose();
    }
}
