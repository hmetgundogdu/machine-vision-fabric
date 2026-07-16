using System.Text.Json;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class ResidentCameraStubIntegrationModuleTests
{
    [Fact]
    public async Task OpenSession_StreamsBoundedResidentFramesFromMemory()
    {
        var root = Path.Combine(Path.GetTempPath(), "mvf-tests", Guid.NewGuid().ToString("N"));
        var imagesRoot = Path.Combine(root, "assets", "images");
        Directory.CreateDirectory(imagesRoot);

        try
        {
            File.WriteAllText(Path.Combine(imagesRoot, "frame-a.png"), "frame-a");
            File.WriteAllText(Path.Combine(imagesRoot, "frame-b.png"), "frame-b");

            var module = new MachineVisionFabric.Integrations.ResidentCameraStub.ResidentCameraStubIntegrationModule();
            await using var session = module.OpenSession(
                JsonSerializer.SerializeToElement(new MachineVisionFabric.Integrations.ResidentCameraStub.ResidentCameraStubOptions
                {
                    SourceFolder = "assets/images",
                    CameraId = "resident-cam-test",
                    Loop = true,
                    FrameIntervalMs = 1,
                    MaxFrames = 3,
                    DeliverMemoryFrames = true
                }),
                root);

            var frames = new List<IFrameEnvelope>();
            await foreach (var frame in session.ReadFramesAsync(CancellationToken.None))
            {
                frames.Add(frame);
                if (frames.Count == 3)
                {
                    break;
                }
            }

            Assert.Equal(1, session.DeclaredCameraCount);
            Assert.Equal(3, session.EstimatedFrameCount);
            Assert.Equal(["resident-cam-test", "resident-cam-test", "resident-cam-test"], frames.Select(frame => frame.CameraId).ToArray());

            await using var stream = await frames[0].OpenReadAsync(CancellationToken.None);
            using var reader = new StreamReader(stream);
            Assert.Equal("frame-a", await reader.ReadToEndAsync());
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
