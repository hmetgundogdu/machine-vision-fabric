using MachineVisionFabric.Contracts.Execution;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Runtime.Execution.NodeRunners;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class ForkPrimitiveNodeRunnerTests
{
    [Fact]
    public async Task ExecuteAsync_WithFrame_EmitsOnAllOutputPorts()
    {
        var frame = MakeFrame("cam1", 1);
        var runner = new ForkPrimitiveNodeRunner("fork1", ["out0", "out1", "out2"]);
        await runner.ActivateAsync(CancellationToken.None);

        var inputs = Inputs("frame", PortValue.FromFrame(frame));
        var result = await runner.ExecuteAsync(inputs, CancellationToken.None);

        Assert.True(result.HasOutput);
        Assert.NotNull(result.Get("out0"));
        Assert.NotNull(result.Get("out1"));
        Assert.NotNull(result.Get("out2"));
        Assert.Same(frame, result.Get("out0")!.Frame);
    }

    [Fact]
    public async Task ExecuteAsync_WithoutFrame_ReturnsNoOutput()
    {
        var runner = new ForkPrimitiveNodeRunner("fork1", ["out0", "out1"]);
        await runner.ActivateAsync(CancellationToken.None);

        var result = await runner.ExecuteAsync(NodeExecutionInputs.Empty, CancellationToken.None);

        Assert.False(result.HasOutput);
    }

    [Fact]
    public async Task ExecuteAsync_NoOutputPorts_ReturnsNoOutput()
    {
        var runner = new ForkPrimitiveNodeRunner("fork1", []);
        await runner.ActivateAsync(CancellationToken.None);

        var inputs = Inputs("frame", PortValue.FromFrame(MakeFrame("cam1", 1)));
        var result = await runner.ExecuteAsync(inputs, CancellationToken.None);

        Assert.False(result.HasOutput);
    }

    private static NodeExecutionInputs Inputs(string portName, PortValue value) =>
        new(new Dictionary<string, PortValue> { [portName] = value });

    private static IFrameEnvelope MakeFrame(string cameraId, int seq) =>
        new Core.Frames.BinaryFrameEnvelope(cameraId, seq, $"frame{seq}.jpg", [(byte)seq]);
}
