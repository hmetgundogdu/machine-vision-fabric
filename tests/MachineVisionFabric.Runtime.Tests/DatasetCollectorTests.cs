using MachineVisionFabric.Contracts.Control;
using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Contracts.Processing;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Core.Frames;
using MachineVisionFabric.Storage;

namespace MachineVisionFabric.Runtime.Tests;

public sealed class DatasetCollectorTests
{
    [Fact]
    public async Task CollectAsync_CopiesFramesAndWritesSessionMetadata()
    {
        var root = Path.Combine(Path.GetTempPath(), "mvf-tests", Guid.NewGuid().ToString("N"));
        var sourceRoot = Path.Combine(root, "source");
        var sessionRoot = Path.Combine(root, "session");

        Directory.CreateDirectory(sourceRoot);
        Directory.CreateDirectory(sessionRoot);

        try
        {
            var sourceA = Path.Combine(sourceRoot, "frame-a.jpg");
            var sourceB = Path.Combine(sourceRoot, "frame-b.png");
            File.WriteAllText(sourceA, "frame-a");
            File.WriteAllText(sourceB, "frame-b");

            var collector = new DatasetCollector();
            var result = await collector.CollectAsync(
                sessionRoot,
                new FabricProfileManifest(),
                declaredCameraCount: 1,
                new FakeProductPresenceGate(true),
                frameProcessor: null,
                new FakeFrameSourceSession(
                    new FileFrameEnvelope("sim-cam-1", 1, sourceA, "frame-a.jpg"),
                    new FileFrameEnvelope("sim-cam-1", 2, sourceB, "frame-b.png")),
                CancellationToken.None);

            Assert.Equal(2, result.CapturedFrameCount);
            Assert.True(File.Exists(result.SessionMetadataPath));
            Assert.Equal(2, Directory.GetFiles(Path.Combine(sessionRoot, "images")).Length);
            Assert.Equal(2, Directory.GetFiles(Path.Combine(sessionRoot, "metadata"), "*.json").Length);
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
    public async Task CollectAsync_SkipsCapture_WhenProductIsNotPresentAndPolicyRequiresIt()
    {
        var root = Path.Combine(Path.GetTempPath(), "mvf-tests", Guid.NewGuid().ToString("N"));
        var sourceRoot = Path.Combine(root, "source");
        var sessionRoot = Path.Combine(root, "session");

        Directory.CreateDirectory(sourceRoot);
        Directory.CreateDirectory(sessionRoot);

        try
        {
            var sourceA = Path.Combine(sourceRoot, "frame-a.jpg");
            File.WriteAllText(sourceA, "frame-a");

            var collector = new DatasetCollector();
            var result = await collector.CollectAsync(
                sessionRoot,
                new FabricProfileManifest
                {
                    CapturePolicy = new DatasetCapturePolicy
                    {
                        RequireProductPresent = true
                    }
                },
                declaredCameraCount: 1,
                new FakeProductPresenceGate(false),
                frameProcessor: null,
                new FakeFrameSourceSession(new FileFrameEnvelope("sim-cam-1", 1, sourceA, "frame-a.jpg")),
                CancellationToken.None);

            Assert.Equal(0, result.CapturedFrameCount);
            Assert.Empty(Directory.GetFiles(Path.Combine(sessionRoot, "images")));
            Assert.True(File.Exists(result.SessionMetadataPath));
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
    public async Task CollectAsync_WritesFramesFromInMemoryPayloads()
    {
        var root = Path.Combine(Path.GetTempPath(), "mvf-tests", Guid.NewGuid().ToString("N"));
        var sessionRoot = Path.Combine(root, "session");

        Directory.CreateDirectory(sessionRoot);

        try
        {
            var collector = new DatasetCollector();
            var result = await collector.CollectAsync(
                sessionRoot,
                new FabricProfileManifest(),
                declaredCameraCount: 1,
                new FakeProductPresenceGate(true),
                frameProcessor: null,
                new FakeFrameSourceSession(
                    new BinaryFrameEnvelope("mem-cam-1", 1, "frame-0001.png", "frame-a"u8.ToArray(), "image/png")),
                CancellationToken.None);

            Assert.Equal(1, result.CapturedFrameCount);

            var storedFile = Directory.GetFiles(Path.Combine(sessionRoot, "images"), "*.png").Single();
            Assert.Equal("frame-a", await File.ReadAllTextAsync(storedFile));
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
    public async Task CollectAsync_CapturesTriggerWindowAroundFirstPositiveGate()
    {
        var root = Path.Combine(Path.GetTempPath(), "mvf-tests", Guid.NewGuid().ToString("N"));
        var sessionRoot = Path.Combine(root, "session");

        Directory.CreateDirectory(sessionRoot);

        try
        {
            var collector = new DatasetCollector();
            var result = await collector.CollectAsync(
                sessionRoot,
                new FabricProfileManifest
                {
                    CapturePolicy = new DatasetCapturePolicy
                    {
                        Mode = "trigger-window",
                        RequireProductPresent = true,
                        PreTriggerFramesPerCamera = 2,
                        PostTriggerFramesPerCamera = 2,
                        GateEvaluationIntervalFrames = 1
                    }
                },
                declaredCameraCount: 1,
                new FakeProductPresenceGate(false, false, false, true, true),
                frameProcessor: null,
                new FakeFrameSourceSession(
                    new BinaryFrameEnvelope("sim-cam-1", 1, "frame-0001.png", "frame-1"u8.ToArray(), "image/png"),
                    new BinaryFrameEnvelope("sim-cam-1", 2, "frame-0002.png", "frame-2"u8.ToArray(), "image/png"),
                    new BinaryFrameEnvelope("sim-cam-1", 3, "frame-0003.png", "frame-3"u8.ToArray(), "image/png"),
                    new BinaryFrameEnvelope("sim-cam-1", 4, "frame-0004.png", "frame-4"u8.ToArray(), "image/png"),
                    new BinaryFrameEnvelope("sim-cam-1", 5, "frame-0005.png", "frame-5"u8.ToArray(), "image/png"),
                    new BinaryFrameEnvelope("sim-cam-1", 6, "frame-0006.png", "frame-6"u8.ToArray(), "image/png")),
                CancellationToken.None);

            Assert.True(result.ProductPresenceDecision.ProductPresent);
            Assert.Equal(5, result.CapturedFrameCount);
            Assert.Equal([2, 3, 4, 5, 6], result.Records.Select(record => record.SequenceNumber).ToArray());
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
    public async Task CollectAsync_DoesNotCapture_WhenTriggerWindowNeverSeesPositiveGate()
    {
        var root = Path.Combine(Path.GetTempPath(), "mvf-tests", Guid.NewGuid().ToString("N"));
        var sessionRoot = Path.Combine(root, "session");

        Directory.CreateDirectory(sessionRoot);

        try
        {
            var collector = new DatasetCollector();
            var result = await collector.CollectAsync(
                sessionRoot,
                new FabricProfileManifest
                {
                    CapturePolicy = new DatasetCapturePolicy
                    {
                        Mode = "trigger-window",
                        RequireProductPresent = true,
                        PreTriggerFramesPerCamera = 1,
                        PostTriggerFramesPerCamera = 1
                    }
                },
                declaredCameraCount: 1,
                new FakeProductPresenceGate(false, false, false),
                frameProcessor: null,
                new FakeFrameSourceSession(
                    new BinaryFrameEnvelope("sim-cam-1", 1, "frame-0001.png", "frame-1"u8.ToArray(), "image/png"),
                    new BinaryFrameEnvelope("sim-cam-1", 2, "frame-0002.png", "frame-2"u8.ToArray(), "image/png"),
                    new BinaryFrameEnvelope("sim-cam-1", 3, "frame-0003.png", "frame-3"u8.ToArray(), "image/png")),
                CancellationToken.None);

            Assert.False(result.ProductPresenceDecision.ProductPresent);
            Assert.Equal(0, result.CapturedFrameCount);
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
    public async Task CollectAsync_SkipsFramesRejectedByFrameProcessor()
    {
        var root = Path.Combine(Path.GetTempPath(), "mvf-tests", Guid.NewGuid().ToString("N"));
        var sessionRoot = Path.Combine(root, "session");

        Directory.CreateDirectory(sessionRoot);

        try
        {
            var collector = new DatasetCollector();
            var result = await collector.CollectAsync(
                sessionRoot,
                new FabricProfileManifest(),
                declaredCameraCount: 1,
                new FakeProductPresenceGate(true),
                new FakeFrameProcessor(true, false, true),
                new FakeFrameSourceSession(
                    new BinaryFrameEnvelope("sim-cam-1", 1, "frame-0001.png", "frame-1"u8.ToArray(), "image/png"),
                    new BinaryFrameEnvelope("sim-cam-1", 2, "frame-0002.png", "frame-2"u8.ToArray(), "image/png"),
                    new BinaryFrameEnvelope("sim-cam-1", 3, "frame-0003.png", "frame-3"u8.ToArray(), "image/png")),
                CancellationToken.None);

            Assert.Equal(2, result.CapturedFrameCount);
            Assert.Equal([1, 3], result.Records.Select(record => record.SequenceNumber).ToArray());
        }
        finally
        {
            if (Directory.Exists(root))
            {
                Directory.Delete(root, recursive: true);
            }
        }
    }

    private sealed class FakeFrameSourceSession(params IFrameEnvelope[] frames) : IFrameSourceSession
    {
        public int DeclaredCameraCount => frames
            .Select(frame => frame.CameraId)
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .DefaultIfEmpty("sim-cam-1")
            .Count();

        public int? EstimatedFrameCount => frames.Length;

        public async IAsyncEnumerable<IFrameEnvelope> ReadFramesAsync([System.Runtime.CompilerServices.EnumeratorCancellation] CancellationToken cancellationToken)
        {
            foreach (var frame in frames)
            {
                cancellationToken.ThrowIfCancellationRequested();
                yield return frame;
                await Task.Yield();
            }
        }

        public ValueTask DisposeAsync() => ValueTask.CompletedTask;
    }

    private sealed class FakeProductPresenceGate(params bool[] decisions) : IProductPresenceGate
    {
        private int evaluationCount;

        public Task<ProductPresenceDecision> EvaluateAsync(CancellationToken cancellationToken)
        {
            cancellationToken.ThrowIfCancellationRequested();

            var currentIndex = decisions.Length == 0
                ? 0
                : Math.Min(evaluationCount, decisions.Length - 1);
            var currentValue = decisions.Length == 0 || decisions[currentIndex];
            evaluationCount++;

            return Task.FromResult(new ProductPresenceDecision(
                currentValue,
                "fake-gate",
                "station-1",
                DateTime.UtcNow,
                $"evaluation={evaluationCount}"));
        }
    }

    private sealed class FakeFrameProcessor(params bool[] decisions) : IFrameProcessor
    {
        private int evaluationCount;

        public Task<FrameProcessorDecision> EvaluateAsync(IFrameEnvelope frame, CancellationToken cancellationToken)
        {
            cancellationToken.ThrowIfCancellationRequested();

            var currentIndex = decisions.Length == 0
                ? 0
                : Math.Min(evaluationCount, decisions.Length - 1);
            var currentValue = decisions.Length == 0 || decisions[currentIndex];
            evaluationCount++;

            return Task.FromResult(new FrameProcessorDecision(
                currentValue,
                "fake-processor",
                "unit-test",
                DateTime.UtcNow,
                $"evaluation={evaluationCount}"));
        }
    }
}
