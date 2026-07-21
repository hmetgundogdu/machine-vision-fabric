using System.Runtime.CompilerServices;
using Mvf.Abstractions;

namespace Mvf.Abstractions.Frames;

public sealed class FileSequenceFrameSourceSession : IFrameSourceSession
{
    private readonly IReadOnlyList<string> filePaths;
    private readonly int declaredCameraCount;
    private readonly int frameIntervalMs;
    private readonly bool loop;

    public FileSequenceFrameSourceSession(
        IReadOnlyList<string> filePaths,
        int declaredCameraCount,
        int frameIntervalMs,
        bool loop)
    {
        this.filePaths = filePaths;
        this.declaredCameraCount = Math.Max(1, declaredCameraCount);
        this.frameIntervalMs = Math.Max(0, frameIntervalMs);
        this.loop = loop;
    }

    public int DeclaredCameraCount => declaredCameraCount;

    public int? EstimatedFrameCount => filePaths.Count * declaredCameraCount;

    public async IAsyncEnumerable<IFrameEnvelope> ReadFramesAsync([EnumeratorCancellation] CancellationToken cancellationToken)
    {
        if (filePaths.Count == 0)
        {
            yield break;
        }

        var sequenceNumbers = new int[declaredCameraCount];

        do
        {
            for (var fileIndex = 0; fileIndex < filePaths.Count; fileIndex++)
            {
                for (var cameraIndex = 1; cameraIndex <= declaredCameraCount; cameraIndex++)
                {
                    cancellationToken.ThrowIfCancellationRequested();

                    var sequenceNumber = ++sequenceNumbers[cameraIndex - 1];
                    var sourcePath = filePaths[fileIndex];

                    yield return new FileFrameEnvelope(
                        $"sim-cam-{cameraIndex}",
                        sequenceNumber,
                        sourcePath,
                        Path.GetFileName(sourcePath),
                        DateTime.UtcNow);

                    if (frameIntervalMs > 0)
                    {
                        await Task.Delay(frameIntervalMs, cancellationToken);
                    }
                }
            }
        }
        while (loop);
    }

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;
}
