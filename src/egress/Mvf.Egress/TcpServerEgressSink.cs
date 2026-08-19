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

    public TcpServerEgressSink(int port, EgressStreams streams = EgressStreams.State)
    {
        _streams = streams;
        _listener = new TcpListener(IPAddress.Loopback, port);
        _listener.Start();
        Port = ((IPEndPoint)_listener.LocalEndpoint).Port;

        _acceptLoop = Task.Run(AcceptLoopAsync);
        _drainLoop = Task.Run(() => _queue.DrainAsync(BroadcastAsync, _cts.Token));
    }

    /// <summary>The bound port (useful when constructed with port 0 for an ephemeral port).</summary>
    public int Port { get; }

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
