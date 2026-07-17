using MachineVisionFabric.Contracts.Execution;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Core.Frames;
using MachineVisionFabric.Runtime.Execution.NodeRunners;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class IfPrimitiveNodeRunnerTests
{
    [Fact]
    public async Task ExecuteAsync_WhenGateOpen_PassesFrameThrough()
    {
        var runner = new IfPrimitiveNodeRunner("branch1");
        await runner.ActivateAsync(CancellationToken.None);

        var frame = new BinaryFrameEnvelope("cam1", 1, "frame.jpg", [0x01, 0x02]);
        var inputs = new NodeExecutionInputs(new Dictionary<string, PortValue>(StringComparer.OrdinalIgnoreCase)
        {
            ["frame"] = PortValue.FromFrame(frame),
            ["productPresent"] = PortValue.FromControl(new ControlSignal
            {
                SignalType = "boolean-gate",
                Value = true
            })
        });

        var result = await runner.ExecuteAsync(inputs, CancellationToken.None);

        Assert.True(result.HasOutput);
        var accepted = result.Get("acceptedFrame");
        Assert.NotNull(accepted);
        Assert.Same(frame, accepted.Frame);
    }

    [Fact]
    public async Task ExecuteAsync_WhenGateClosed_EmitsNoOutput()
    {
        var runner = new IfPrimitiveNodeRunner("branch1");
        await runner.ActivateAsync(CancellationToken.None);

        var frame = new BinaryFrameEnvelope("cam1", 1, "frame.jpg", [0x01]);
        var inputs = new NodeExecutionInputs(new Dictionary<string, PortValue>(StringComparer.OrdinalIgnoreCase)
        {
            ["frame"] = PortValue.FromFrame(frame),
            ["productPresent"] = PortValue.FromControl(new ControlSignal
            {
                SignalType = "boolean-gate",
                Value = false
            })
        });

        var result = await runner.ExecuteAsync(inputs, CancellationToken.None);

        Assert.False(result.HasOutput);
    }

    [Fact]
    public async Task ExecuteAsync_WhenFrameMissing_EmitsNoOutput()
    {
        var runner = new IfPrimitiveNodeRunner("branch1");
        await runner.ActivateAsync(CancellationToken.None);

        var inputs = new NodeExecutionInputs(new Dictionary<string, PortValue>(StringComparer.OrdinalIgnoreCase)
        {
            ["productPresent"] = PortValue.FromControl(new ControlSignal
            {
                SignalType = "boolean-gate",
                Value = true
            })
        });

        var result = await runner.ExecuteAsync(inputs, CancellationToken.None);

        Assert.False(result.HasOutput);
    }

    [Fact]
    public async Task ExecuteAsync_WhenControlMissing_EmitsNoOutput()
    {
        var runner = new IfPrimitiveNodeRunner("branch1");
        await runner.ActivateAsync(CancellationToken.None);

        var frame = new BinaryFrameEnvelope("cam1", 1, "frame.jpg", [0x01]);
        var inputs = new NodeExecutionInputs(new Dictionary<string, PortValue>(StringComparer.OrdinalIgnoreCase)
        {
            ["frame"] = PortValue.FromFrame(frame)
        });

        var result = await runner.ExecuteAsync(inputs, CancellationToken.None);

        Assert.False(result.HasOutput);
    }
}
