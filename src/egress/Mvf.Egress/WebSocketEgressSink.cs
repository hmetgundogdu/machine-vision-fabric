using System.Net;
using System.Net.WebSockets;
using Mvf.Graph.Execution;

namespace Mvf.Egress;

/// <summary>
/// An <see cref="IEgressSink"/> that serves the same framed records over <b>WebSocket</b> — the transport a
/// <b>browser</b> observer needs (a browser cannot open a raw UDP/TCP socket). Each record is sent as one
/// binary WS message carrying the record <b>body</b> (the 4-byte length prefix is dropped, since the WS
/// message boundary already frames it), so a browser decodes each message directly with <c>decodeRecord</c>.
/// Built on <see cref="HttpListener"/> — no ASP.NET dependency. Publish stays off the hot path via
/// <see cref="EgressRecordQueue"/>.
/// </summary>
public sealed class WebSocketEgressSink : IEgressSink
{
    private readonly EgressRecordQueue _queue = new();
    private readonly EgressStreams _streams;
    private readonly HttpListener _listener;
    private readonly CancellationTokenSource _cts = new();
    private readonly List<WebSocket> _clients = [];
    private readonly object _clientsLock = new();
    private readonly Task _acceptLoop;
    private readonly Task _drainLoop;

    // Kept outside the ring and replayed per subscriber — see TcpServerEgressSink.
    private volatile byte[]? _topologyFrame;

    /// <param name="bindAddress">
    /// Which interface to serve on; defaults to loopback for the same reason as
    /// <see cref="TcpServerEgressSink"/>. "0.0.0.0" / "*" become the HttpListener wildcard prefix, which on
    /// Windows needs an urlacl reservation or an elevated process — a failure to start here is that,
    /// not a bug.
    /// </param>
    public WebSocketEgressSink(int port, EgressStreams streams = EgressStreams.State, string? bindAddress = null)
    {
        _streams = streams;
        Port = port;
        _listener = new HttpListener();
        var host = bindAddress switch
        {
            null or "" or "127.0.0.1" or "localhost" => "127.0.0.1",
            "0.0.0.0" or "*" or "+" => "+",
            var explicitHost => explicitHost,
        };
        _listener.Prefixes.Add($"http://{host}:{port}/");
        _listener.Start();

        _acceptLoop = Task.Run(AcceptLoopAsync);
        _drainLoop = Task.Run(() => _queue.DrainAsync(BroadcastAsync, _cts.Token));
    }

    public int Port { get; }

    /// <summary>Retained for late subscribers and broadcast through the ring for attached ones — see
    /// <see cref="TcpServerEgressSink.PublishTopology"/>.</summary>
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
                return _clients.Count(ws => ws.State == WebSocketState.Open);
            }
        }
    }

    private async Task AcceptLoopAsync()
    {
        try
        {
            while (!_cts.IsCancellationRequested)
            {
                var context = await _listener.GetContextAsync().ConfigureAwait(false);
                if (!context.Request.IsWebSocketRequest)
                {
                    context.Response.StatusCode = 400;
                    context.Response.Close();
                    continue;
                }

                var wsContext = await context.AcceptWebSocketAsync(subProtocol: null).ConfigureAwait(false);

                // Send topology before registering, so it cannot interleave with the drain loop's sends.
                var preamble = _topologyFrame;
                if (preamble is not null)
                {
                    try
                    {
                        await wsContext.WebSocket.SendAsync(
                            new ReadOnlyMemory<byte>(preamble, 4, preamble.Length - 4),
                            WebSocketMessageType.Binary,
                            endOfMessage: true,
                            _cts.Token).ConfigureAwait(false);
                    }
                    catch (Exception ex) when (ex is WebSocketException or OperationCanceledException or ObjectDisposedException)
                    {
                        wsContext.WebSocket.Dispose();
                        continue;
                    }
                }

                lock (_clientsLock)
                {
                    _clients.Add(wsContext.WebSocket);
                }
            }
        }
        catch (OperationCanceledException) { }
        catch (ObjectDisposedException) { }
        catch (HttpListenerException) { }
        catch (InvalidOperationException) { }
    }

    private async Task BroadcastAsync(byte[] frame)
    {
        // WS messages are self-delimiting: send the record body (drop the 4-byte length prefix).
        var body = new ReadOnlyMemory<byte>(frame, 4, frame.Length - 4);

        List<WebSocket> snapshot;
        lock (_clientsLock)
        {
            if (_clients.Count == 0)
            {
                return;
            }

            snapshot = [.. _clients];
        }

        foreach (var socket in snapshot)
        {
            if (socket.State != WebSocketState.Open)
            {
                Remove(socket);
                continue;
            }

            try
            {
                await socket.SendAsync(body, WebSocketMessageType.Binary, endOfMessage: true, _cts.Token).ConfigureAwait(false);
            }
            catch
            {
                Remove(socket);
            }
        }
    }

    private void Remove(WebSocket socket)
    {
        lock (_clientsLock)
        {
            _clients.Remove(socket);
        }

        try
        {
            socket.Dispose();
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
            _listener.Close();
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
            // shutting down
        }

        lock (_clientsLock)
        {
            foreach (var socket in _clients)
            {
                try
                {
                    socket.Dispose();
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
