using System.Text.Json;
using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Contracts.Simulation;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Runtime;
using MachineVisionFabric.Sources.Simulators;
using Microsoft.Extensions.Logging.Abstractions;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class FrameSourceResolverTests
{
    [Fact]
    public async Task Resolve_UsesExternalSourceModule_WhenProfileRequestsModule()
    {
        var root = Path.Combine(Path.GetTempPath(), "mvf-tests", Guid.NewGuid().ToString("N"));
        var imagesRoot = Path.Combine(root, "assets", "images");
        Directory.CreateDirectory(imagesRoot);

        try
        {
            File.WriteAllText(Path.Combine(imagesRoot, "frame-a.jpg"), "frame-a");
            File.WriteAllText(Path.Combine(imagesRoot, "frame-b.png"), "frame-b");

            var resolver = new ProfileFrameSourceResolver(
                new FolderSequenceSourceCatalog(),
                new FakeLoader(new MachineVisionFabric.Integrations.FolderSource.FolderSourceIntegrationModule()),
                NullLogger<ProfileFrameSourceResolver>.Instance);

            var profile = new FabricRuntimeProfile
            {
                Source = new SourceBinding
                {
                    Mode = "module",
                    ModuleId = "mvf.folder-source",
                    Config = JsonDocument.Parse(
                        """
                        {
                          "sourceFolder": "assets/images",
                          "loop": false,
                          "parallelCameraCount": 2
                        }
                        """).RootElement
                }
            };

            var resolution = resolver.Resolve(profile, root, integrationsRoot: ".");
            await using var session = resolution.Session;
            var frames = await ReadFramesAsync(session, maximumFrames: 10);

            Assert.Equal("module", resolution.Strategy);
            Assert.Equal("mvf.folder-source", resolution.Source);
            Assert.Equal(2, session.DeclaredCameraCount);
            Assert.Equal(4, frames.Count);
        }
        finally
        {
            if (Directory.Exists(root))
            {
                Directory.Delete(root, recursive: true);
            }
        }
    }

    [Fact]
    public async Task Resolve_FallsBackToBuiltInSimulatorSource_WhenProfileUsesBuiltinMode()
    {
        var root = Path.Combine(Path.GetTempPath(), "mvf-tests", Guid.NewGuid().ToString("N"));
        var imagesRoot = Path.Combine(root, "assets", "images");
        Directory.CreateDirectory(imagesRoot);

        try
        {
            File.WriteAllText(Path.Combine(imagesRoot, "frame-a.jpg"), "frame-a");

            var resolver = new ProfileFrameSourceResolver(
                new FolderSequenceSourceCatalog(),
                new FakeLoader(),
                NullLogger<ProfileFrameSourceResolver>.Instance);

            var profile = new FabricRuntimeProfile
            {
                SimulatorSource = new FolderSequenceSourceOptions
                {
                    SourceFolder = "assets/images",
                    Loop = false,
                    ParallelCameraCount = 3
                }
            };

            var resolution = resolver.Resolve(profile, root, integrationsRoot: ".");
            await using var session = resolution.Session;
            var frames = await ReadFramesAsync(session, maximumFrames: 10);

            Assert.Equal("builtin", resolution.Strategy);
            Assert.Equal("builtin-folder-sequence", resolution.Source);
            Assert.Equal(3, session.DeclaredCameraCount);
            Assert.Equal(3, frames.Count);
        }
        finally
        {
            if (Directory.Exists(root))
            {
                Directory.Delete(root, recursive: true);
            }
        }
    }

    private sealed class FakeLoader(params MachineVisionFabric.Contracts.Integrations.IIntegrationModule[] modules) : IIntegrationModuleLoader
    {
        public IReadOnlyList<MachineVisionFabric.Contracts.Integrations.IIntegrationModule> LoadModules(string pluginRoot) => modules;
    }

    private static async Task<List<IFrameEnvelope>> ReadFramesAsync(IFrameSourceSession session, int maximumFrames)
    {
        var frames = new List<IFrameEnvelope>();

        await foreach (var frame in session.ReadFramesAsync(CancellationToken.None))
        {
            frames.Add(frame);
            if (frames.Count >= maximumFrames)
            {
                break;
            }
        }

        return frames;
    }
}
