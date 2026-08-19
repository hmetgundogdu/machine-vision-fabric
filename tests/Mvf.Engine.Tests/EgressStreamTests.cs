using System.Net;
using System.Net.Sockets;
using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Egress;
using Mvf.Engine.Execution;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Tests;

public sealed class EgressStreamTests
{
    [Fact]
    public void Wire_RoundTrips_CycleRecord()
    {
        var runId = Guid.NewGuid().ToString("N");
        var progress = new PipelineExecutionProgress
        {
            RunId = runId,
            CycleIndex = 4,
            TotalCycles = 5,
            AcceptedCycles = 3,
            CycleAccepted = true,
            Elapsed = TimeSpan.FromMilliseconds(1234),
        };

        var frame = EgressWire.EncodeCycle(progress, seq: 7);
        // strip the u32 length prefix before decoding the body
        var decoded = EgressWire.Decode(frame.AsSpan(4));

        Assert.Equal(EgressStreamKind.Cycle, decoded.Kind);
        Assert.Equal(Guid.ParseExact(runId, "N"), decoded.RunId);
        Assert.Equal(4u, decoded.CycleIndex);
        Assert.Equal(7u, decoded.Seq);
        Assert.Equal(5u, decoded.TotalCycles);
        Assert.Equal(3u, decoded.AcceptedCycles);
        Assert.True(decoded.Accepted);
        Assert.Equal(1234, decoded.ElapsedMillis);
    }

    [Fact]
    public void Wire_RoundTrips_NodeTransitionRecord()
    {
        var runId = Guid.NewGuid().ToString("N");
        var nodeEvent = new NodeExecutionEvent
        {
            RunId = runId,
            NodeId = "classify1",
            CycleIndex = 2,
            HasOutput = true,
            Faulted = false,
            DurationMicros = 987,
            OutputFrameBytes = 4096,
            OutputPortNames = ["frame"],
            InputPortNames = ["frame"],
        };

        var frame = EgressWire.EncodeNodeTransition(nodeEvent, seq: 11);
        var decoded = EgressWire.Decode(frame.AsSpan(4));

        Assert.Equal(EgressStreamKind.NodeTransition, decoded.Kind);
        Assert.Equal("classify1", decoded.NodeId);
        Assert.Equal("frame", decoded.Port);
        Assert.Equal(2u, decoded.CycleIndex);
        Assert.Equal(11u, decoded.Seq);
        Assert.Equal(987, decoded.DurationMicros);
        Assert.True(decoded.HasOutput);
        Assert.False(decoded.Faulted);
        Assert.Equal(4096, decoded.OutputFrameBytes);
        Assert.False(decoded.HasPayload);
    }

    [Fact]
    public void Wire_RoundTrips_NodeTransitionFrameWithPayload()
    {
        var runId = Guid.NewGuid().ToString("N");
        var nodeEvent = new NodeExecutionEvent
        {
            RunId = runId,
            NodeId = "cam",
            CycleIndex = 1,
            HasOutput = true,
            Faulted = false,
            DurationMicros = 500,
            OutputFrameBytes = 3,
            OutputPortNames = ["frame"],
            InputPortNames = [],
        };

        var header = new byte[PayloadDescriptor.HeaderSize];
        new PayloadDescriptor(PayloadMediaType.Image, PayloadElementType.UInt8, new long[] { 3 }).WriteHeader(header);
        byte[] payload = [10, 20, 30];

        var frame = EgressWire.EncodeNodeTransitionFrame(nodeEvent, seq: 3, header, payload);
        var decoded = EgressWire.Decode(frame.AsSpan(4));

        Assert.True(decoded.HasPayload);
        Assert.Equal(PayloadMediaType.Image, decoded.MediaType);
        Assert.Equal(PayloadElementType.UInt8, decoded.ElementType);
        Assert.Equal(payload, decoded.Payload);
        Assert.Equal(new long[] { 3 }, decoded.PayloadShape);
    }

    [Fact]
    public async Task Executor_WithFrameEgress_StreamsFramePayload()
    {
        var frames = Enumerable.Range(1, 3)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.jpg", [(byte)i, (byte)(i + 100)]))
            .ToArray();

        var activator = new FakeActivator(
            ("source1", new FakeSource("source1", frames)),
            ("sink1", new FakeSink("sink1")));

        using var sink = new TcpServerEgressSink(port: 0, streams: EgressStreams.All);

        using var client = new TcpClient();
        await client.ConnectAsync(IPAddress.Loopback, sink.Port);
        await WaitUntilAsync(() => sink.Stats.Subscribers == 1, TimeSpan.FromSeconds(2));

        var reader = new EgressStreamReader(client.GetStream());
        var received = new List<DecodedEgressRecord>();
        using var readCts = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        var readTask = Task.Run(async () =>
        {
            try
            {
                while (!readCts.IsCancellationRequested)
                {
                    var record = await reader.ReadAsync(readCts.Token);
                    if (record is null || received.Count(r => r.Kind == EgressStreamKind.Cycle) >= 3)
                    {
                        break;
                    }

                    received.Add(record);
                }
            }
            catch (OperationCanceledException)
            {
            }
        });

        var executor = new PipelineGraphExecutor(activator);
        var report = await executor.ExecuteAsync(
            BuildSourceSinkDefinition(),
            new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = ".", EgressSink = sink },
            CancellationToken.None);

        Assert.True(report.Succeeded);
        await readTask;

        var framePayloads = received
            .Where(r => r.Kind == EgressStreamKind.NodeTransition && r.NodeId == "source1" && r.HasPayload)
            .ToList();

        Assert.NotEmpty(framePayloads);
        Assert.All(framePayloads, r => Assert.Equal(2, r.Payload!.Length));
        // first cycle emitted [1, 101]
        Assert.Contains(framePayloads, r => r.Payload![0] == 1 && r.Payload![1] == 101);
    }

    [Fact]
    public async Task Executor_WithTcpEgress_StreamsCycleAndNodeStateRecords()
    {
        var frames = Enumerable.Range(1, 3)
            .Select(i => (IFrameEnvelope)new BinaryFrameEnvelope("cam1", i, $"f{i}.jpg", [(byte)i, (byte)i]))
            .ToArray();

        var activator = new FakeActivator(
            ("source1", new FakeSource("source1", frames)),
            ("sink1", new FakeSink("sink1")));

        using var sink = new TcpServerEgressSink(port: 0);

        using var client = new TcpClient();
        await client.ConnectAsync(IPAddress.Loopback, sink.Port);
        await WaitUntilAsync(() => sink.Stats.Subscribers == 1, TimeSpan.FromSeconds(2));

        var reader = new EgressStreamReader(client.GetStream());
        var received = new List<DecodedEgressRecord>();
        using var readCts = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        var readTask = Task.Run(async () =>
        {
            try
            {
                while (!readCts.IsCancellationRequested)
                {
                    var record = await reader.ReadAsync(readCts.Token);
                    if (record is null)
                    {
                        break;
                    }

                    received.Add(record);
                    if (received.Count(r => r.Kind == EgressStreamKind.Cycle) >= 3)
                    {
                        break;
                    }
                }
            }
            catch (OperationCanceledException)
            {
                // read window elapsed — assertions below report what did arrive
            }
        });

        var executor = new PipelineGraphExecutor(activator);
        var report = await executor.ExecuteAsync(
            BuildSourceSinkDefinition(),
            new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = ".", EgressSink = sink },
            CancellationToken.None);

        Assert.True(report.Succeeded);

        await readTask;

        var cycles = received.Where(r => r.Kind == EgressStreamKind.Cycle).ToList();
        var nodes = received.Where(r => r.Kind == EgressStreamKind.NodeTransition).ToList();

        Assert.Equal(3, cycles.Count);
        Assert.Contains(nodes, n => n.NodeId == "source1" && n.HasOutput && n.OutputFrameBytes == 2);
        Assert.Contains(nodes, n => n.NodeId == "sink1");
        Assert.Single(received.Select(r => r.RunId).Distinct());
        Assert.True(sink.Stats.Published >= received.Count);
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

    private static PipelineDefinition BuildSourceSinkDefinition() =>
        new()
        {
            Name = "egress-test-graph",
            Nodes =
            [
                new PipelineNodeDefinition
                {
                    Id = "source1", Kind = "integration-module", Category = "source",
                    Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "frame" }]
                },
                new PipelineNodeDefinition
                {
                    Id = "sink1", Kind = "runtime-builtin", Category = "output", BuiltinType = "dataset-writer",
                    Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "frame" }]
                }
            ],
            Edges =
            [
                new PipelineEdgeDefinition
                {
                    Id = "e1", Kind = "data",
                    From = new PipelinePortReference { NodeId = "source1", Port = "frame" },
                    To = new PipelinePortReference { NodeId = "sink1", Port = "frame" }
                }
            ]
        };

    private sealed class FakeActivator(params (string NodeId, INodeRunner Runner)[] runners) : IPipelineNodeActivator
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

    private sealed class FakeSource(string nodeId, IReadOnlyList<IFrameEnvelope> frames) : INodeRunner
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

    private sealed class FakeSink(string nodeId) : INodeRunner
    {
        public string NodeId { get; } = nodeId;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            Task.FromResult(NodeExecutionResult.NoOutput);

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
}
