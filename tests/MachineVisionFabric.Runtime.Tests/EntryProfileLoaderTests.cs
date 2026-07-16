using MachineVisionFabric.Runtime;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class EntryProfileLoaderTests
{
    [Fact]
    public async Task LoadAsync_LoadsSimulatorSourceFromEntryProfile()
    {
        var root = Path.Combine(Path.GetTempPath(), "mvf-tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);

        try
        {
            var profilePath = Path.Combine(root, "profile.json");
            await File.WriteAllTextAsync(
                profilePath,
                """
                {
                  "name": "sample-profile",
                  "mode": "dataset-collection-profile",
                  "description": "Sample profile",
                  "capabilities": ["dataset-capture", "simulator-source"],
                  "simulatorSource": {
                    "sourceFolder": "assets/images",
                    "loop": false,
                    "frameIntervalMs": 500,
                    "parallelCameraCount": 3
                  }
                }
                """);

            var loader = new EntryProfileLoader();
            var result = await loader.LoadAsync(root, "profile.json", CancellationToken.None);

            Assert.Equal("sample-profile", result.Name);
            Assert.Equal("assets/images", result.SimulatorSource.SourceFolder);
            Assert.False(result.SimulatorSource.Loop);
            Assert.Equal(500, result.SimulatorSource.FrameIntervalMs);
            Assert.Equal(3, result.SimulatorSource.ParallelCameraCount);
        }
        finally
        {
            if (Directory.Exists(root))
            {
                Directory.Delete(root, recursive: true);
            }
        }
    }
}
