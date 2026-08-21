using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Engine.Execution;
using Mvf.Engine.Modules;
using Mvf.Engine.Plugins;
using Mvf.Graph.Simulation;
using Mvf.Hosting.Worker;
using Mvf.Transport.SharedMemory;

namespace Mvf.Engine.Tests;

/// <summary>
/// The analyzer path end to end: a multi-output node whose module is <c>runtime: python</c> is
/// activated through the real <see cref="PipelineNodeActivator"/> into a <c>FrameAnalyzerNodeRunner</c>.
/// The worker reads the input frame from the arena, writes a derived frame back into it, and returns
/// both control and structured inference outputs in the same cycle. Requires python3 on PATH.
/// </summary>
public sealed class PythonAnalyzerAutoWireTests
{
    [Fact]
    public async Task AnalyzeNode_WithPythonAnalyzer_ProducesVisualControlAndStructuredOutputs()
    {
        var repo = FindRepoRoot();

        using var dataPlane = new SharedMemoryArena();
        var activator = new PipelineNodeActivator(
            new IntegrationModuleLoader(),
            new EmptySimulatorSourceCatalog(),
            new ModuleCatalog(),
            new StdioModuleHost(dataPlane));

        var node = new PipelineNodeDefinition
        {
            Id = "inspect1",
            Kind = "integration-module",
            Category = "analyze",
            ModuleId = "py.brightness-analyzer",
            Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }],
            Outputs =
            [
                new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame", Required = false },
                new PipelinePortDefinition { Name = "class", Channel = "control", DataType = "control/classification", Required = false },
                new PipelinePortDefinition { Name = "result", Channel = "control", DataType = "control/value:json", Required = false }
            ]
        };

        var options = new PipelineExecutionOptions
        {
            PackageRoot = repo,
            IntegrationsRoot = Path.Combine(repo, "modules")
        };

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));

        var runner = await activator.ActivateAsync(node, options, cts.Token);
        await using var _ = runner;

        var result = await runner.ExecuteAsync(FrameInputs(2, [10, 20, 30]), cts.Token);

        var outputFrame = result.Get("frame")?.Frame;
        Assert.NotNull(outputFrame);
        Assert.Equal([10, 20, 30], await ReadAllBytesAsync(outputFrame!, cts.Token));

        var signal = result.Get("class")?.Control;
        Assert.NotNull(signal);
        Assert.Equal("ok", signal!.ClassLabel);
        Assert.Equal(20d, signal.Measurement);
        Assert.Equal("mean-byte", signal.Unit);

        var value = result.Get("result")?.Control;
        Assert.NotNull(value);
        var payload = Assert.IsType<JsonObject>(value!.Payload);
        Assert.Equal("ok", payload["label"]!.GetValue<string>());
        Assert.Equal(20d, payload["mean"]!.GetValue<double>());
        Assert.Equal(2, payload["sequence"]!.GetValue<int>());
    }

    private static NodeExecutionInputs FrameInputs(int seq, byte[] data) =>
        new(new Dictionary<string, PortValue>(StringComparer.OrdinalIgnoreCase)
        {
            ["frame"] = PortValue.FromFrame(new BinaryFrameEnvelope("cam1", seq, $"f{seq}.bmp", data, "image/bmp"))
        });

    private static async Task<byte[]> ReadAllBytesAsync(IFrameEnvelope frame, CancellationToken cancellationToken)
    {
        await using var stream = await frame.OpenReadAsync(cancellationToken);
        using var buffer = new MemoryStream();
        await stream.CopyToAsync(buffer, cancellationToken);
        return buffer.ToArray();
    }

    private static string FindRepoRoot()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null)
        {
            if (File.Exists(Path.Combine(dir.FullName, "CLAUDE.md"))
                && Directory.Exists(Path.Combine(dir.FullName, "modules")))
            {
                return dir.FullName;
            }
            dir = dir.Parent;
        }

        throw new InvalidOperationException("Repository root (with CLAUDE.md + modules/) not found.");
    }
}
