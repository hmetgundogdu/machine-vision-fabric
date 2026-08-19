using System.Net;
using System.Net.Sockets;
using Mvf.Abstractions;
using Mvf.Egress;
using Mvf.Engine.Execution;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;

namespace Mvf.Engine.Tests;

/// <summary>
/// Covers the seam that lets a frame carry its own typed shape onto the egress wire. Without it every heap
/// frame is published as a flat run of bytes, and a consumer holding a 320x240 image cannot tell it from a
/// vector of 76 800 numbers.
/// </summary>
public sealed class EgressFrameDescriptorTests
{
    [Fact]
    public async Task AFrameThatDeclaresItsShape_StreamsWithIt()
    {
        var records = await StreamOneFrameAsync(
            new DescribedFrame(
                pixels: new byte[8 * 4],
                descriptor: new PayloadDescriptor(PayloadMediaType.Image, PayloadElementType.UInt8, [4L, 8L])));

        var frame = Assert.Single(records, r => r.HasPayload);
        Assert.Equal(PayloadMediaType.Image, frame.MediaType);
        Assert.Equal(PayloadElementType.UInt8, frame.ElementType);
        Assert.Equal([4L, 8L], frame.PayloadShape!);
    }

    [Fact]
    public async Task ADescriptorThatDoesNotAccountForTheBytes_IsIgnored()
    {
        // 4x8 uint8 is 32 bytes, but the frame carries 32 - 1. Trusting the declaration here would hand a
        // consumer a shape it can read past the end of, so the publisher falls back to a flat blob.
        var records = await StreamOneFrameAsync(
            new DescribedFrame(
                pixels: new byte[(8 * 4) - 1],
                descriptor: new PayloadDescriptor(PayloadMediaType.Image, PayloadElementType.UInt8, [4L, 8L])));

        // The media type still comes from the content type; it is the *shape* that falls back to flat.
        var frame = Assert.Single(records, r => r.HasPayload);
        Assert.Equal([31L], frame.PayloadShape!);
    }

    [Fact]
    public async Task AFrameWithNoDescriptor_StaysAFlatBlob()
    {
        var records = await StreamOneFrameAsync(new DescribedFrame(pixels: new byte[16], descriptor: null));

        var frame = Assert.Single(records, r => r.HasPayload);
        Assert.Equal([16L], frame.PayloadShape!);
    }

    private static async Task<List<DecodedEgressRecord>> StreamOneFrameAsync(IFrameEnvelope frame)
    {
        using var sink = new TcpServerEgressSink(port: 0, streams: EgressStreams.State | EgressStreams.Frame);

        using var client = new TcpClient();
        await client.ConnectAsync(IPAddress.Loopback, sink.Port);
        await WaitUntilAsync(() => sink.Stats.Subscribers == 1, TimeSpan.FromSeconds(2));

        var reading = ReadUntilPayloadAsync(client);

        var executor = new PipelineGraphExecutor(new SingleSourceActivator(frame));
        var report = await executor.ExecuteAsync(
            Definition(),
            new PipelineExecutionOptions { PackageRoot = ".", IntegrationsRoot = ".", EgressSink = sink },
            CancellationToken.None);

        Assert.True(report.Succeeded);
        return await reading;
    }

    private static async Task<List<DecodedEgressRecord>> ReadUntilPayloadAsync(TcpClient client)
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
                if (record.HasPayload)
                {
                    break;
                }
            }
        }
        catch (OperationCanceledException)
        {
            // read window elapsed — the assertions report what did arrive
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

    private static PipelineDefinition Definition() =>
        new()
        {
            Name = "descriptor-test-graph",
            Nodes =
            [
                new PipelineNodeDefinition
                {
                    Id = "cam", Kind = "integration-module", Category = "source",
                    Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "frame" }]
                },
                new PipelineNodeDefinition
                {
                    Id = "sink", Kind = "runtime-builtin", Category = "output", BuiltinType = "dataset-writer",
                    Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "frame" }]
                }
            ],
            Edges =
            [
                new PipelineEdgeDefinition
                {
                    Id = "e1", Kind = "data",
                    From = new PipelinePortReference { NodeId = "cam", Port = "frame" },
                    To = new PipelinePortReference { NodeId = "sink", Port = "frame" }
                }
            ]
        };

    /// <summary>A frame that reports whatever descriptor the test hands it — including a wrong one.</summary>
    private sealed class DescribedFrame(byte[] pixels, PayloadDescriptor? descriptor) : IFrameEnvelope
    {
        public string CameraId => "cam";

        public int SequenceNumber => 0;

        public string FileName => "frame.gray";

        public string? SourcePath => null;

        public DateTime TimestampUtc { get; } = DateTime.UtcNow;

        public string ContentType => "image/x-raw-gray8";

        public long? ContentLength => pixels.LongLength;

        public PayloadDescriptor? Descriptor => descriptor;

        public ValueTask<Stream> OpenReadAsync(CancellationToken cancellationToken) =>
            ValueTask.FromResult<Stream>(new MemoryStream(pixels, writable: false));
    }

    private sealed class SingleSourceActivator(IFrameEnvelope frame) : IPipelineNodeActivator
    {
        public Task<INodeRunner> ActivateAsync(
            PipelineNodeDefinition node, PipelineExecutionOptions options, CancellationToken cancellationToken) =>
            Task.FromResult<INodeRunner>(node.Id == "cam" ? new OneFrameSource(frame) : new NullSink(node.Id));
    }

    private sealed class OneFrameSource(IFrameEnvelope frame) : INodeRunner
    {
        private bool _emitted;

        public string NodeId => "cam";

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken)
        {
            if (_emitted)
            {
                return Task.FromResult(NodeExecutionResult.NoOutput);
            }

            _emitted = true;
            return Task.FromResult(NodeExecutionResult.Single("frame", PortValue.FromFrame(frame)));
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class NullSink(string nodeId) : INodeRunner
    {
        public string NodeId { get; } = nodeId;

        public Task ActivateAsync(CancellationToken cancellationToken) => Task.CompletedTask;

        public Task<NodeExecutionResult> ExecuteAsync(NodeExecutionInputs inputs, CancellationToken cancellationToken) =>
            Task.FromResult(NodeExecutionResult.NoOutput);

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }
}
