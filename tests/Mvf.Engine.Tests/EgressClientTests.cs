using Mvf.Egress;
using Mvf.Egress.Client;
using Mvf.Graph.Execution;

namespace Mvf.Engine.Tests;

public sealed class EgressClientTests
{
    [Fact]
    public async Task StreamTcpAsync_ReceivesPublishedRecords()
    {
        using var sink = new TcpServerEgressSink(port: 0, streams: EgressStreams.State);

        var received = new List<DecodedEgressRecord>();
        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        var consume = Task.Run(async () =>
        {
            await foreach (var record in EgressClient.StreamTcpAsync("127.0.0.1", sink.Port, cts.Token))
            {
                received.Add(record);
                if (received.Count >= 2)
                {
                    break;
                }
            }
        }, cts.Token);

        await WaitUntilAsync(() => sink.Stats.Subscribers == 1, TimeSpan.FromSeconds(2));

        var runId = Guid.NewGuid().ToString("N");
        sink.PublishCycle(new PipelineExecutionProgress
        {
            RunId = runId, CycleIndex = 0, TotalCycles = 1, AcceptedCycles = 1, CycleAccepted = true,
            Elapsed = TimeSpan.FromMilliseconds(3),
        });
        sink.PublishNodeTransition(new NodeExecutionEvent
        {
            RunId = runId, NodeId = "cam", CycleIndex = 0, HasOutput = true, Faulted = false,
            DurationMicros = 42, OutputFrameBytes = 128, OutputPortNames = ["frame"], InputPortNames = [],
        });

        await consume;

        Assert.Equal(2, received.Count);
        Assert.Equal(EgressStreamKind.Cycle, received[0].Kind);
        Assert.Equal(EgressStreamKind.NodeTransition, received[1].Kind);
        Assert.Equal("cam", received[1].NodeId);
        Assert.Equal(128, received[1].OutputFrameBytes);
    }

    [Fact]
    public void EdgeRegistry_ExpiresStaleEdges()
    {
        var registry = new EgressEdgeRegistry();
        var beacon = new EgressBeaconInfo
        {
            EdgeId = "panel-1", Pipeline = "p", Transport = "tcp", Port = 8791, Streams = "state", Status = "running",
        };

        registry.Apply(beacon, nowMs: 1000);
        Assert.Single(registry.Live(ttlMs: 3000, nowMs: 2000)); // fresh
        Assert.Empty(registry.Live(ttlMs: 3000, nowMs: 5000)); // 4s old > 3s ttl → dropped
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
