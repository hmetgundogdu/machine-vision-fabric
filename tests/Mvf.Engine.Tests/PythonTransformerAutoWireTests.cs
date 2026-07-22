using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Engine.Execution;
using Mvf.Engine.Modules;
using Mvf.Engine.Plugins;
using Mvf.Hosting.Worker;
using Mvf.Transport.SharedMemory;

namespace Mvf.Engine.Tests;

/// <summary>
/// The transformer path end to end: a compute node whose module is <c>runtime: python</c> is activated
/// through the real <see cref="PipelineNodeActivator"/> into a FrameTransformerNodeRunner. The worker
/// reads the input frame from the arena and writes a NEW frame back into it (no base64), and the engine
/// hands back an arena-born frame. Requires python3 on PATH.
/// </summary>
public sealed class PythonTransformerAutoWireTests
{
    [Fact]
    public async Task ComputeNode_WithPythonTransformer_ProducesANewFrameOverSharedMemory()
    {
        var repo = FindRepoRoot();

        using var dataPlane = new SharedMemoryArena();
        var activator = new PipelineNodeActivator(
            new IntegrationModuleLoader(),
            new EmptySimulatorSourceCatalog(),
            new ModuleCatalog(),
            new StdioModuleHost(dataPlane));

        // A lean `{ "id": "inv1", "module": "py.invert-transformer" }` expands to exactly this.
        var node = new PipelineNodeDefinition
        {
            Id = "inv1",
            Kind = "integration-module",
            Category = "compute",
            ModuleId = "py.invert-transformer",
            Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }],
            Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
        };

        var options = new PipelineExecutionOptions
        {
            PackageRoot = repo,
            IntegrationsRoot = Path.Combine(repo, "modules")
        };

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));

        var runner = await activator.ActivateAsync(node, options, cts.Token);
        await using var _ = runner;

        var result = await runner.ExecuteAsync(FrameInputs(1, [10, 20, 30]), cts.Token);
        var output = result.Get("frame")?.Frame;
        Assert.NotNull(output);

        var bytes = await ReadAllBytesAsync(output!, cts.Token);
        Assert.Equal([245, 235, 225], bytes); // 255 - b, inverted in Python and read back from the arena
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
