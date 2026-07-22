using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Engine.Execution;
using Mvf.Engine.Modules;
using Mvf.Engine.Plugins;
using Mvf.Hosting.Worker;

namespace Mvf.Engine.Tests;

/// <summary>
/// The polyglot auto-wiring path end to end: a classify node whose module is <c>runtime: python</c>
/// is activated through the real <see cref="PipelineNodeActivator"/>. The activator reads the
/// runtime from the manifest catalog, spawns the worker via <see cref="StdioModuleHost"/>, and
/// drops it into a FrameClassifierNodeRunner — no .NET plugin, no bespoke wiring in the pipeline.
/// Requires python3 on PATH.
/// </summary>
public sealed class PythonModuleAutoWireTests
{
    [Fact]
    public async Task ClassifyNode_WithPythonModule_AutoWiresWorkerAndClassifies()
    {
        var repo = FindRepoRoot();

        var activator = new PipelineNodeActivator(
            new IntegrationModuleLoader(),
            new EmptySimulatorSourceCatalog(),
            new ModuleCatalog(),
            new StdioModuleHost());

        // A lean `{ "id": "bright1", "module": "py.brightness-classifier" }` expands to exactly this.
        var node = new PipelineNodeDefinition
        {
            Id = "bright1",
            Kind = "integration-module",
            Category = "classify",
            ModuleId = "py.brightness-classifier",
            Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }],
            Outputs = [new PipelinePortDefinition { Name = "class", Channel = "control", DataType = "control/classification" }]
        };

        var options = new PipelineExecutionOptions
        {
            PackageRoot = repo,
            IntegrationsRoot = Path.Combine(repo, "modules")
        };

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));

        var runner = await activator.ActivateAsync(node, options, cts.Token);
        await using var _ = runner;

        var dark = await runner.ExecuteAsync(FrameInputs(1, new byte[64]), cts.Token); // all-zero → black
        var darkSignal = dark.Get("class")?.Control;
        Assert.NotNull(darkSignal);
        Assert.Equal("black", darkSignal!.ClassLabel);
        Assert.Equal(0d, darkSignal.Measurement);

        var bright = await runner.ExecuteAsync(
            FrameInputs(2, Enumerable.Repeat((byte)255, 64).ToArray()), cts.Token); // all-255 → ok
        var brightSignal = bright.Get("class")?.Control;
        Assert.NotNull(brightSignal);
        Assert.Equal("ok", brightSignal!.ClassLabel);
        Assert.Equal(255d, brightSignal.Measurement);
    }

    private static NodeExecutionInputs FrameInputs(int seq, byte[] data) =>
        new(new Dictionary<string, PortValue>(StringComparer.OrdinalIgnoreCase)
        {
            ["frame"] = PortValue.FromFrame(new BinaryFrameEnvelope("cam1", seq, $"f{seq}.bmp", data, "image/bmp"))
        });

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
