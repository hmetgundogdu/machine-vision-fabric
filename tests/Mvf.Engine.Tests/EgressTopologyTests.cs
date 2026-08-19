using System.Net;
using System.Net.Sockets;
using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Egress;
using Mvf.Engine.Execution;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Tests;

/// <summary>
/// Covers the topology record (wire v2): what it carries, what it deliberately does not, and the two ways a
/// subscriber can come by it — through the live stream, or replayed on attach.
/// </summary>
public sealed class EgressTopologyTests
{
    [Fact]
    public void Wire_RoundTrips_TopologyRecord()
    {
        var runId = Guid.NewGuid().ToString("N");
        var topology = EgressTopology.FromDefinition(BuildDefinition(), runId);

        var frame = EgressWire.EncodeTopology(topology, runId, seq: 3);
        var decoded = EgressWire.Decode(frame.AsSpan(4));

        Assert.Equal(EgressStreamKind.Topology, decoded.Kind);
        Assert.Equal(Guid.ParseExact(runId, "N"), decoded.RunId);
        Assert.Equal(3u, decoded.Seq);

        var t = decoded.Topology;
        Assert.NotNull(t);
        Assert.Equal("egress-topology-graph", t!.Name);
        // The run id travels in the header; a decoded topology must still carry it, or a consumer holding
        // only the topology cannot say which run it describes.
        Assert.Equal(runId, t.RunId);

        var source = Assert.Single(t.Nodes, n => n.Id == "cam1");
        Assert.Equal("source", source.Category);
        Assert.Equal("folder-source", source.ModuleId);
        Assert.Null(source.PrimitiveType);
        var outPort = Assert.Single(source.Outputs);
        Assert.Equal("frame", outPort.Name);
        Assert.Equal("data", outPort.Channel);

        var edge = Assert.Single(t.Edges, e => e.Kind == "control");
        Assert.Equal("route1", edge.FromNode);
        Assert.Equal("class", edge.FromPort);
        Assert.Equal("sink1", edge.ToNode);
    }

    [Fact]
    public void Topology_CarriesStructureButNeverConfig()
    {
        var definition = BuildDefinition();
        definition.Nodes[0].Config = new System.Text.Json.Nodes.JsonObject
        {
            ["password"] = "hunter2",
            ["dir"] = "/mnt/secret-frames",
        };

        var runId = Guid.NewGuid().ToString("N");
        var frame = EgressWire.EncodeTopology(EgressTopology.FromDefinition(definition, runId), runId, seq: 0);

        // The whole record, bytes and all: nothing from Config may appear on a stream that is announced on
        // the LAN and served to whoever attaches.
        var wire = System.Text.Encoding.UTF8.GetString(frame);
        Assert.DoesNotContain("hunter2", wire, StringComparison.Ordinal);
        Assert.DoesNotContain("secret-frames", wire, StringComparison.Ordinal);
        Assert.Contains("cam1", wire, StringComparison.Ordinal); // structure did travel
    }

    [Fact]
    public void Topology_RebuildsARenderableDefinition()
    {
        var runId = Guid.NewGuid().ToString("N");
        var original = BuildDefinition();

        var rebuilt = EgressTopology.FromDefinition(original, runId).ToDefinition();

        Assert.Equal(original.Name, rebuilt.Name);
        Assert.Equal(original.Nodes.Count, rebuilt.Nodes.Count);
        Assert.Equal(original.Edges.Count, rebuilt.Edges.Count);

        var rebuiltSink = Assert.Single(rebuilt.Nodes, n => n.Id == "sink1");
        Assert.Equal("dataset-writer", rebuiltSink.BuiltinType);

        // Both ports survive with their channels: the data/control split is structure, not decoration, and
        // an observer that lost it would draw the wrong graph.
        Assert.Equal(2, rebuiltSink.Inputs.Count);
        Assert.Equal("data", Assert.Single(rebuiltSink.Inputs, p => p.Name == "frame").Channel);
        Assert.Equal("control", Assert.Single(rebuiltSink.Inputs, p => p.Name == "class").Channel);

        var rebuiltEdge = Assert.Single(rebuilt.Edges, e => e.Id == "e1");
        Assert.Equal("cam1", rebuiltEdge.From.NodeId);
        Assert.Equal("sink1", rebuiltEdge.To.NodeId);
    }

    [Fact]
    public void Decoder_AcceptsAV1Producer()
    {
        // A v1 producer never sends topology; its state records must still read cleanly, so an observer can
        // attach to an engine older than this build and fall back to the node table.
        var progress = new PipelineExecutionProgress
        {
            RunId = Guid.NewGuid().ToString("N"),
            CycleIndex = 1,
            TotalCycles = 1,
            AcceptedCycles = 1,
            CycleAccepted = true,
            Elapsed = TimeSpan.FromMilliseconds(10),
        };

        var frame = EgressWire.EncodeCycle(progress, seq: 0);
        frame[6] = 1; // version byte: 4-byte length prefix + u16 magic

        var decoded = EgressWire.Decode(frame.AsSpan(4));

        Assert.Equal(EgressStreamKind.Cycle, decoded.Kind);
        Assert.Null(decoded.Topology);
    }

    [Fact]
    public void Decoder_RejectsAFutureVersionLoudly()
    {
        var progress = new PipelineExecutionProgress
        {
            RunId = Guid.NewGuid().ToString("N"),
            CycleIndex = 0,
            TotalCycles = 0,
            AcceptedCycles = 0,
            CycleAccepted = false,
            Elapsed = TimeSpan.Zero,
        };

        var frame = EgressWire.EncodeCycle(progress, seq: 0);
        frame[6] = 99;

        // Loudly, rather than reading an unknown layout as if it were understood.
        Assert.Throws<InvalidDataException>(() => EgressWire.Decode(frame.AsSpan(4)));
    }

    [Fact]
    public async Task Subscriber_AttachedBeforeTheRun_ReceivesTopology()
    {
        using var sink = new TcpServerEgressSink(port: 0);

        using var client = new TcpClient();
        await client.ConnectAsync(IPAddress.Loopback, sink.Port);
        await WaitUntilAsync(() => sink.Stats.Subscribers == 1, TimeSpan.FromSeconds(2));

        var records = ReadRecordsAsync(client, stopAfter: r => r.Kind == EgressStreamKind.Topology);

        var executor = new PipelineGraphExecutor(BuildActivator());
        var report = await executor.ExecuteAsync(
            BuildDefinition(),
            new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = ".", EgressSink = sink },
            CancellationToken.None);

        Assert.True(report.Succeeded);

        var received = await records;
        var topology = Assert.Single(received, r => r.Kind == EgressStreamKind.Topology);
        Assert.Equal("egress-topology-graph", topology.Topology!.Name);
        Assert.Contains(topology.Topology.Nodes, n => n.Id == "cam1");
    }

    [Fact]
    public async Task Subscriber_AttachedMidRun_ReceivesTheReplayedTopologyFirst()
    {
        using var sink = new TcpServerEgressSink(port: 0);

        // The run publishes topology while nobody is attached.
        var executor = new PipelineGraphExecutor(BuildActivator());
        var report = await executor.ExecuteAsync(
            BuildDefinition(),
            new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = ".", EgressSink = sink },
            CancellationToken.None);
        Assert.True(report.Succeeded);

        using var late = new TcpClient();
        await late.ConnectAsync(IPAddress.Loopback, sink.Port);

        var received = await ReadRecordsAsync(late, stopAfter: r => r.Kind == EgressStreamKind.Topology);

        var first = Assert.Single(received);
        Assert.Equal(EgressStreamKind.Topology, first.Kind);
        Assert.Equal("egress-topology-graph", first.Topology!.Name);
    }

    private static async Task<List<DecodedEgressRecord>> ReadRecordsAsync(
        TcpClient client, Func<DecodedEgressRecord, bool> stopAfter)
    {
        var reader = new EgressStreamReader(client.GetStream());
        var received = new List<DecodedEgressRecord>();
        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(5));

        try
        {
            while (!cts.IsCancellationRequested)
            {
                var record = await reader.ReadAsync(cts.Token);
                if (record is null)
                {
                    break;
                }

                received.Add(record);
                if (stopAfter(record))
                {
                    break;
                }
            }
        }
        catch (OperationCanceledException)
        {
            // read window elapsed — assertions report what did arrive
        }

        return received;
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

    private static IPipelineNodeActivator BuildActivator()
    {
        var frames = Enumerable.Range(1, 2)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.jpg", [(byte)i]))
            .ToArray();

        return new TopologyFakeActivator(
            ("cam1", new TopologyFakeSource("cam1", frames)),
            ("route1", new TopologyFakeSink("route1")),
            ("sink1", new TopologyFakeSink("sink1")));
    }

    private static PipelineDefinition BuildDefinition() =>
        new()
        {
            Name = "egress-topology-graph",
            Nodes =
            [
                new PipelineNodeDefinition
                {
                    Id = "cam1", Kind = "integration-module", Category = "source", ModuleId = "folder-source",
                    Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "frame" }]
                },
                new PipelineNodeDefinition
                {
                    Id = "route1", Kind = "embedded-primitive", Category = "flow", PrimitiveType = "switch",
                    Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "frame" }],
                    Outputs = [new PipelinePortDefinition { Name = "class", Channel = "control", DataType = "class" }]
                },
                new PipelineNodeDefinition
                {
                    Id = "sink1", Kind = "runtime-builtin", Category = "output", BuiltinType = "dataset-writer",
                    Inputs =
                    [
                        new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "frame" },
                        new PipelinePortDefinition { Name = "class", Channel = "control", DataType = "class", Required = false }
                    ]
                }
            ],
            Edges =
            [
                new PipelineEdgeDefinition
                {
                    Id = "e1", Kind = "data",
                    From = new PipelinePortReference { NodeId = "cam1", Port = "frame" },
                    To = new PipelinePortReference { NodeId = "sink1", Port = "frame" }
                },
                new PipelineEdgeDefinition
                {
                    Id = "e2", Kind = "data",
                    From = new PipelinePortReference { NodeId = "cam1", Port = "frame" },
                    To = new PipelinePortReference { NodeId = "route1", Port = "frame" }
                },
                new PipelineEdgeDefinition
                {
                    Id = "e3", Kind = "control",
                    From = new PipelinePortReference { NodeId = "route1", Port = "class" },
                    To = new PipelinePortReference { NodeId = "sink1", Port = "class" }
                }
            ]
        };

    private sealed class TopologyFakeActivator(params (string NodeId, INodeRunner Runner)[] runners)
        : IPipelineNodeActivator
    {
        private readonly Dictionary<string, INodeRunner> _runners =
            runners.ToDictionary(r => r.NodeId, r => r.Runner, StringComparer.OrdinalIgnoreCase);

        public async Task<INodeRunner> ActivateAsync(
            PipelineNodeDefinition node,
            PipelineExecutionOptions options,
            CancellationToken cancellationToken)
        {
            var runner = _runners[node.Id];
            await runner.ActivateAsync(cancellationToken);
            return runner;
        }
    }

    private sealed class TopologyFakeSource(string nodeId, IReadOnlyList<IFrameEnvelope> frames) : INodeRunner
    {
        private int _index;

        public string NodeId { get; } = nodeId;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            _index >= frames.Count
                ? Task.FromResult(NodeExecutionResult.NoOutput)
                : Task.FromResult(NodeExecutionResult.Single("frame", PortValue.FromFrame(frames[_index++])));

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class TopologyFakeSink(string nodeId) : INodeRunner
    {
        public string NodeId { get; } = nodeId;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            Task.FromResult(NodeExecutionResult.NoOutput);

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
}
