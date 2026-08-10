using System.Text.Json.Nodes;
using Mvf.Abstractions;
using Mvf.Abstractions.Frames;
using Mvf.Engine.Execution;
using Mvf.Engine.Modules;
using Mvf.Engine.Plugins;
using Mvf.Graph.Execution;
using Mvf.Graph.Pipelines;
using Mvf.Hosting.Worker;
using Mvf.Transport.SharedMemory;

namespace Mvf.Engine.Tests;

/// <summary>
/// Config reaching an out-of-process module, and reaching it again while it runs.
///
/// <para>Until the <c>configure</c> message existed this whole path was a hole: a node's config was
/// resolved, type-checked, stored and offered to the operator as a live tunable, and the worker never
/// saw a byte of it — a binding on a Python node appeared to work and changed nothing. These go through
/// the real activator, the real stdio host and a real Python child, because every layer in that chain
/// had to be taught to carry it and a stub would prove none of them.</para>
///
/// <para>Requires python3 on PATH, like the other Python auto-wire tests.</para>
/// </summary>
public sealed class LiveConfigureTests
{
    [Fact]
    public async Task ANodesConfig_ReachesThePythonWorkerBeforeItsFirstFrame()
    {
        using var dataPlane = new SharedMemoryArena();
        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));

        var runner = await ActivateTunableAsync(dataPlane, offset: 5, cts.Token);
        await using var _ = runner;

        Assert.Equal([15, 25, 35], await TransformAsync(runner, [10, 20, 30], cts.Token));
    }

    [Fact]
    public async Task EditingTheConfig_TakesEffectOnTheNextFrameWithoutReopeningTheWorker()
    {
        using var dataPlane = new SharedMemoryArena();
        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));

        var runner = await ActivateTunableAsync(dataPlane, offset: 5, cts.Token);
        await using var _ = runner;
        Assert.Equal([15], await TransformAsync(runner, [10], cts.Token));

        var reconfigurable = Assert.IsAssignableFrom<IReconfigurable>(runner);
        Assert.True(reconfigurable.CanReconfigure);
        Assert.True(await reconfigurable.TryReconfigureAsync(
            new JsonObject { ["offset"] = 100 }, cts.Token));

        // Same child process, new answer. The offset lives in module state that a restart would reset,
        // so reading 110 here is also the proof that nothing was re-opened underneath us.
        Assert.Equal([110], await TransformAsync(runner, [10], cts.Token));
    }

    [Fact]
    public async Task AModuleThatRefusesTheConfig_ReportsItRatherThanAcceptingItSilently()
    {
        using var dataPlane = new SharedMemoryArena();
        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));

        var runner = await ActivateTunableAsync(dataPlane, offset: 5, cts.Token);
        await using var _ = runner;

        var reconfigurable = Assert.IsAssignableFrom<IReconfigurable>(runner);
        await Assert.ThrowsAnyAsync<Exception>(() => reconfigurable.TryReconfigureAsync(
            new JsonObject { ["offset"] = "not-a-number" }, cts.Token));

        // …and the module is still serving on the config it had, not on a half-applied one.
        Assert.Equal([15], await TransformAsync(runner, [10], cts.Token));
    }

    [Fact]
    public async Task AModuleThatNeverDeclaredTheFeature_IsNotOfferedAsReconfigurable()
    {
        // py.invert-transformer has no on_configure, so its hello carries no `features`. Reporting it as
        // reconfigurable would have the engine send a message the SDK's loop silently ignores, and wait
        // for a reply that never arrives — a hang, not an error.
        using var dataPlane = new SharedMemoryArena();
        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(30));

        var runner = await ActivateAsync(dataPlane, "py.invert-transformer", config: null, cts.Token);
        await using var _ = runner;

        var reconfigurable = Assert.IsAssignableFrom<IReconfigurable>(runner);
        Assert.False(reconfigurable.CanReconfigure);
        Assert.False(await reconfigurable.TryReconfigureAsync(
            new JsonObject { ["offset"] = 1 }, cts.Token));
        // and it still works, untouched
        Assert.Equal([245], await TransformAsync(runner, [10], cts.Token));
    }

    // --- harness --------------------------------------------------------------

    private static Task<INodeRunner> ActivateTunableAsync(
        SharedMemoryArena dataPlane, int offset, CancellationToken cancellationToken) =>
        ActivateAsync(dataPlane, "py.tunable-transformer",
                      new JsonObject { ["offset"] = offset }, cancellationToken);

    private static async Task<INodeRunner> ActivateAsync(
        SharedMemoryArena dataPlane, string moduleId, JsonObject? config,
        CancellationToken cancellationToken)
    {
        var repo = FindRepoRoot();
        var activator = new PipelineNodeActivator(
            new IntegrationModuleLoader(),
            new EmptySimulatorSourceCatalog(),
            new ModuleCatalog(),
            new StdioModuleHost(dataPlane));

        var node = new PipelineNodeDefinition
        {
            Id = "tune1",
            Kind = "integration-module",
            Category = "compute",
            ModuleId = moduleId,
            Inputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }],
            Outputs = [new PipelinePortDefinition { Name = "frame", Channel = "data", DataType = "data/frame" }]
        };
        if (config is not null)
        {
            node.Config = config;
        }

        return await activator.ActivateAsync(
            node,
            new PipelineExecutionOptions
            {
                PackageRoot = repo,
                IntegrationsRoot = Path.Combine(repo, "modules")
            },
            cancellationToken);
    }

    private static async Task<byte[]> TransformAsync(
        INodeRunner runner, byte[] input, CancellationToken cancellationToken)
    {
        var inputs = new NodeExecutionInputs(
            new Dictionary<string, PortValue>(StringComparer.OrdinalIgnoreCase)
            {
                ["frame"] = PortValue.FromFrame(
                    new BinaryFrameEnvelope("cam1", 1, "f1.bmp", input, "image/bmp"))
            });

        var result = await runner.ExecuteAsync(inputs, cancellationToken);
        var output = result.Get("frame")?.Frame;
        Assert.NotNull(output);

        await using var stream = await output!.OpenReadAsync(cancellationToken);
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
