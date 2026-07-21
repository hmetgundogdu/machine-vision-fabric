using Mvf.Graph.Processing;
using Mvf.Abstractions;
using Mvf.Engine.Execution.NodeRunners;

namespace Mvf.Engine.Tests;

public sealed class FrameClassifierNodeRunnerTests
{
    [Fact]
    public async Task ExecuteAsync_EmitsClassificationControlSignalOnClassPort()
    {
        var frame = MakeFrame("cam1", 1);
        var classifier = new StubClassifier(new FrameClassification(
            Label: "black",
            Source: "brightness-classifier",
            EvaluatedAtUtc: DateTime.UtcNow,
            Measurement: 3.2,
            Unit: "mean-gray"));
        var runner = new FrameClassifierNodeRunner("classify1", classifier);
        await runner.ActivateAsync(CancellationToken.None);

        var inputs = new NodeExecutionInputs(new Dictionary<string, PortValue>
        {
            ["frame"] = PortValue.FromFrame(frame)
        });

        var result = await runner.ExecuteAsync(inputs, CancellationToken.None);

        Assert.True(result.HasOutput);
        var output = result.Get("class");
        Assert.NotNull(output);
        Assert.True(output!.IsControl);
        Assert.Equal("classification", output.Control!.SignalType);
        Assert.Equal("black", output.Control.ClassLabel);
        Assert.Equal(3.2, output.Control.Measurement);
        Assert.Equal("mean-gray", output.Control.Unit);
        // The classifier must not forward the frame onto the data channel.
        Assert.Null(output.Frame);
    }

    [Fact]
    public async Task ExecuteAsync_ClassificationFeedsSwitchRouting()
    {
        var frame = MakeFrame("cam1", 1);
        var classifier = new StubClassifier(new FrameClassification("reject", "stub", DateTime.UtcNow));
        var classifyRunner = new FrameClassifierNodeRunner("classify1", classifier);
        var switchRunner = new SwitchPrimitiveNodeRunner("switch1",
            new HashSet<string>(["accept", "reject", "default"], StringComparer.OrdinalIgnoreCase));
        await classifyRunner.ActivateAsync(CancellationToken.None);
        await switchRunner.ActivateAsync(CancellationToken.None);

        var classResult = await classifyRunner.ExecuteAsync(
            new NodeExecutionInputs(new Dictionary<string, PortValue>
            {
                ["frame"] = PortValue.FromFrame(frame)
            }),
            CancellationToken.None);

        var switchResult = await switchRunner.ExecuteAsync(
            new NodeExecutionInputs(new Dictionary<string, PortValue>
            {
                ["frame"] = PortValue.FromFrame(frame),
                ["class"] = classResult.Get("class")!
            }),
            CancellationToken.None);

        Assert.True(switchResult.HasOutput);
        Assert.NotNull(switchResult.Get("reject"));
        Assert.Null(switchResult.Get("accept"));
        Assert.Same(frame, switchResult.Get("reject")!.Frame);
    }

    [Fact]
    public async Task ExecuteAsync_NoFrame_ReturnsNoOutput()
    {
        var runner = new FrameClassifierNodeRunner("classify1",
            new StubClassifier(new FrameClassification("x", "stub", DateTime.UtcNow)));
        await runner.ActivateAsync(CancellationToken.None);

        var result = await runner.ExecuteAsync(NodeExecutionInputs.Empty, CancellationToken.None);

        Assert.False(result.HasOutput);
    }

    private sealed class StubClassifier(FrameClassification classification) : IFrameClassifier
    {
        public Task<FrameClassification> ClassifyAsync(IFrameEnvelope frame, CancellationToken cancellationToken) =>
            Task.FromResult(classification);
    }

    private static IFrameEnvelope MakeFrame(string cameraId, int seq) =>
        new Mvf.Abstractions.Frames.BinaryFrameEnvelope(cameraId, seq, $"frame{seq}.jpg", [(byte)seq]);
}
