using Mvf.Graph.Execution;
using Mvf.Abstractions;
using Mvf.Engine.Execution.NodeRunners;

namespace Mvf.Engine.Tests;

public sealed class SwitchPrimitiveNodeRunnerTests
{
    [Fact]
    public async Task ExecuteAsync_ClassMatchesPort_RoutesToCorrectPort()
    {
        var frame = MakeFrame("cam1", 1);
        var runner = new SwitchPrimitiveNodeRunner("switch1",
            new HashSet<string>(["accept", "reject", "default"], StringComparer.OrdinalIgnoreCase));
        await runner.ActivateAsync(CancellationToken.None);

        var inputs = Inputs(frame, classLabel: "accept");
        var result = await runner.ExecuteAsync(inputs, CancellationToken.None);

        Assert.True(result.HasOutput);
        Assert.NotNull(result.Get("accept"));
        Assert.Null(result.Get("reject"));
        Assert.Same(frame, result.Get("accept")!.Frame);
    }

    [Fact]
    public async Task ExecuteAsync_NoMatchFallsToDefault()
    {
        var frame = MakeFrame("cam1", 1);
        var runner = new SwitchPrimitiveNodeRunner("switch1",
            new HashSet<string>(["accept", "reject", "default"], StringComparer.OrdinalIgnoreCase));
        await runner.ActivateAsync(CancellationToken.None);

        var inputs = Inputs(frame, classLabel: "quarantine");
        var result = await runner.ExecuteAsync(inputs, CancellationToken.None);

        Assert.True(result.HasOutput);
        Assert.NotNull(result.Get("default"));
        Assert.Same(frame, result.Get("default")!.Frame);
    }

    [Fact]
    public async Task ExecuteAsync_NoMatchNoDefault_ReturnsNoOutput()
    {
        var frame = MakeFrame("cam1", 1);
        var runner = new SwitchPrimitiveNodeRunner("switch1",
            new HashSet<string>(["accept", "reject"], StringComparer.OrdinalIgnoreCase));
        await runner.ActivateAsync(CancellationToken.None);

        var inputs = Inputs(frame, classLabel: "unknown");
        var result = await runner.ExecuteAsync(inputs, CancellationToken.None);

        Assert.False(result.HasOutput);
    }

    [Fact]
    public async Task ExecuteAsync_MissingClassSignal_ReturnsNoOutput()
    {
        var frame = MakeFrame("cam1", 1);
        var runner = new SwitchPrimitiveNodeRunner("switch1",
            new HashSet<string>(["accept"], StringComparer.OrdinalIgnoreCase));
        await runner.ActivateAsync(CancellationToken.None);

        var inputs = new NodeExecutionInputs(new Dictionary<string, PortValue>
        {
            ["frame"] = PortValue.FromFrame(frame)
            // no "class" port
        });

        var result = await runner.ExecuteAsync(inputs, CancellationToken.None);
        Assert.False(result.HasOutput);
    }

    private static NodeExecutionInputs Inputs(IFrameEnvelope frame, string classLabel) =>
        new(new Dictionary<string, PortValue>
        {
            ["frame"] = PortValue.FromFrame(frame),
            ["class"] = PortValue.FromControl(new ControlSignal
            {
                SignalType = "classification",
                Value = false,
                ClassLabel = classLabel
            })
        });

    private static IFrameEnvelope MakeFrame(string cameraId, int seq) =>
        new Mvf.Abstractions.Frames.BinaryFrameEnvelope(cameraId, seq, $"frame{seq}.jpg", [(byte)seq]);
}
