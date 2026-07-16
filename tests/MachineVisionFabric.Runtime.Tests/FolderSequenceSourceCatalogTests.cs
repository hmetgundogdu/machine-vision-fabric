using MachineVisionFabric.Contracts.Simulation;
using MachineVisionFabric.Sources.Simulators;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class FolderSequenceSourceCatalogTests
{
    [Fact]
    public async Task OpenSession_StreamsFramesForDeclaredCameraCount()
    {
        var root = Path.Combine(Path.GetTempPath(), "mvf-tests", Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(root);

        try
        {
            File.WriteAllText(Path.Combine(root, "a.jpg"), "frame-a");
            File.WriteAllText(Path.Combine(root, "b.png"), "frame-b");

            var catalog = new FolderSequenceSourceCatalog();
            await using var session = catalog.OpenSession(new FolderSequenceSourceOptions
            {
                SourceFolder = root,
                ParallelCameraCount = 2
            });
            var frames = new List<string>();

            await foreach (var frame in session.ReadFramesAsync(CancellationToken.None))
            {
                frames.Add(frame.CameraId);
                if (frames.Count == 4)
                {
                    break;
                }
            }

            Assert.Equal(4, frames.Count);
            Assert.Equal(2, frames.Count(cameraId => cameraId == "sim-cam-1"));
            Assert.Equal(2, frames.Count(cameraId => cameraId == "sim-cam-2"));
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
