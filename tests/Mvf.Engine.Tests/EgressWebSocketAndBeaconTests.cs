using System.Net;
using System.Net.Sockets;
using System.Net.WebSockets;
using Mvf.Egress;
using Mvf.Graph.Execution;

namespace Mvf.Engine.Tests;

public sealed class EgressWebSocketAndBeaconTests
{
    [Fact]
    public async Task WebSocketSink_DeliversRecordsAsBinaryMessages()
    {
        var port = FreePort();
        using var sink = new WebSocketEgressSink(port, EgressStreams.All);

        using var ws = new ClientWebSocket();
        using var connectCts = new CancellationTokenSource(TimeSpan.FromSeconds(3));
        await ws.ConnectAsync(new Uri($"ws://127.0.0.1:{port}/"), connectCts.Token);
        await WaitUntilAsync(() => sink.Stats.Subscribers == 1, TimeSpan.FromSeconds(2));

        var runId = Guid.NewGuid().ToString("N");
        sink.PublishCycle(new PipelineExecutionProgress
        {
            RunId = runId, CycleIndex = 0, TotalCycles = 1, AcceptedCycles = 1, CycleAccepted = true,
            Elapsed = TimeSpan.FromMilliseconds(5),
        });
        sink.PublishNodeTransition(new NodeExecutionEvent
        {
            RunId = runId, NodeId = "cam", CycleIndex = 0, HasOutput = true, Faulted = false,
            DurationMicros = 100, OutputFrameBytes = 64, OutputPortNames = ["frame"], InputPortNames = [],
        });

        using var readCts = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        var first = await ReceiveRecordAsync(ws, readCts.Token);
        var second = await ReceiveRecordAsync(ws, readCts.Token);

        Assert.Equal(EgressStreamKind.Cycle, first.Kind);
        Assert.Equal(Guid.ParseExact(runId, "N"), first.RunId);
        Assert.Equal(EgressStreamKind.NodeTransition, second.Kind);
        Assert.Equal("cam", second.NodeId);
        Assert.Equal(64, second.OutputFrameBytes);
    }

    [Fact]
    public async Task Beacon_AnnouncesAParseableDatagram()
    {
        // Point the beacon at a unicast loopback endpoint so the send/serialize path is exercised
        // deterministically (multicast delivery is environment-dependent; the group is a production const).
        using var listener = new UdpClient(new IPEndPoint(IPAddress.Loopback, 0));
        var target = (IPEndPoint)listener.Client.LocalEndPoint!;

        var info = new EgressBeaconInfo
        {
            EdgeId = "unit-edge",
            Pipeline = "demo-pipeline",
            Transport = "tcp",
            Port = 8791,
            Streams = "state,frame",
            Status = "running",
        };
        using var beacon = new EgressBeacon(info, TimeSpan.FromMilliseconds(100), target);

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(3));
        var datagram = await listener.ReceiveAsync(cts.Token);

        Assert.True(EgressBeacon.TryParse(datagram.Buffer, out var parsed));
        Assert.Equal("unit-edge", parsed.EdgeId);
        Assert.Equal("demo-pipeline", parsed.Pipeline);
        Assert.Equal("tcp", parsed.Transport);
        Assert.Equal(8791, parsed.Port);
        Assert.Equal("state,frame", parsed.Streams);
        Assert.Equal("running", parsed.Status);
    }

    [Fact]
    public void Beacon_TryParse_RejectsUnrelatedDatagram()
    {
        Assert.False(EgressBeacon.TryParse("hello world"u8, out _));
        Assert.False(EgressBeacon.TryParse("{\"foo\":1}"u8, out _));
    }

    private static int FreePort()
    {
        var listener = new TcpListener(IPAddress.Loopback, 0);
        listener.Start();
        var port = ((IPEndPoint)listener.LocalEndpoint).Port;
        listener.Stop();
        return port;
    }

    private static async Task<DecodedEgressRecord> ReceiveRecordAsync(ClientWebSocket ws, CancellationToken cancellationToken)
    {
        var buffer = new byte[64 * 1024];
        using var message = new MemoryStream();
        WebSocketReceiveResult result;
        do
        {
            result = await ws.ReceiveAsync(buffer, cancellationToken);
            message.Write(buffer, 0, result.Count);
        }
        while (!result.EndOfMessage);

        return EgressWire.Decode(message.ToArray());
    }

    private static async Task WaitUntilAsync(Func<bool> condition, TimeSpan timeout)
    {
        var deadline = DateTime.UtcNow + timeout;
        while (DateTime.UtcNow < deadline)
        {
            if (condition())
            {
                return;
            }

            await Task.Delay(20);
        }

        Assert.True(condition(), "condition not met within timeout");
    }
}
