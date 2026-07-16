using System.Runtime.CompilerServices;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;

namespace MachineVisionFabric.Integrations.ResidentCameraStub;

public sealed class ResidentCameraStubSession : BackgroundFrameSourceSession
{
    private static readonly string[] SupportedExtensions = [".jpg", ".jpeg", ".png", ".bmp", ".tif", ".tiff"];

    public ResidentCameraStubSession(string sourceFolder, ResidentCameraStubOptions options)
        : base(
            declaredCameraCount: 1,
            estimatedFrameCount: ResolveEstimatedFrameCount(ResolveFiles(sourceFolder).Length, options),
            boundedCapacity: options.BoundedCapacity)
    {
        SourceFolder = sourceFolder;
        Options = options;
        AvailableFiles = ResolveFiles(sourceFolder);
        StartBackgroundProducer(ProduceFramesAsync);
    }

    public string SourceFolder { get; }

    public ResidentCameraStubOptions Options { get; }

    public IReadOnlyList<string> AvailableFiles { get; }

    private async Task ProduceFramesAsync(CancellationToken cancellationToken)
    {
        if (AvailableFiles.Count == 0)
        {
            return;
        }

        var sequenceNumber = 0;

        do
        {
            foreach (var filePath in AvailableFiles)
            {
                cancellationToken.ThrowIfCancellationRequested();

                sequenceNumber++;
                var frame = await CreateEnvelopeAsync(filePath, sequenceNumber, cancellationToken);
                await PublishAsync(frame, cancellationToken);

                if (Options.MaxFrames is int maxFrames && sequenceNumber >= maxFrames)
                {
                    return;
                }

                if (Options.FrameIntervalMs > 0)
                {
                    await Task.Delay(Options.FrameIntervalMs, cancellationToken);
                }
            }
        }
        while (Options.Loop);
    }

    private async Task<IFrameEnvelope> CreateEnvelopeAsync(string filePath, int sequenceNumber, CancellationToken cancellationToken)
    {
        if (!Options.DeliverMemoryFrames)
        {
            return FrameEnvelopeFactory.FromFile(Options.CameraId, sequenceNumber, filePath, Path.GetFileName(filePath), DateTime.UtcNow);
        }

        await using var stream = File.OpenRead(filePath);
        using var memoryStream = new MemoryStream();
        await stream.CopyToAsync(memoryStream, cancellationToken);

        return FrameEnvelopeFactory.FromBytes(
            Options.CameraId,
            sequenceNumber,
            Path.GetFileName(filePath),
            memoryStream.ToArray(),
            ResolveContentType(filePath),
            DateTime.UtcNow,
            filePath);
    }

    private static int? ResolveEstimatedFrameCount(int fileCount, ResidentCameraStubOptions options)
    {
        if (options.MaxFrames is int maxFrames && maxFrames > 0)
        {
            return maxFrames;
        }

        if (!options.Loop)
        {
            return fileCount;
        }

        return null;
    }

    private static string[] ResolveFiles(string sourceFolder)
    {
        return Directory.Exists(sourceFolder)
            ? Directory
                .EnumerateFiles(sourceFolder, "*.*", SearchOption.TopDirectoryOnly)
                .Where(file => SupportedExtensions.Contains(Path.GetExtension(file), StringComparer.OrdinalIgnoreCase))
                .OrderBy(file => file, StringComparer.OrdinalIgnoreCase)
                .ToArray()
            : [];
    }

    private static string ResolveContentType(string filePath)
    {
        return Path.GetExtension(filePath).ToLowerInvariant() switch
        {
            ".jpg" or ".jpeg" => "image/jpeg",
            ".png" => "image/png",
            ".bmp" => "image/bmp",
            ".tif" or ".tiff" => "image/tiff",
            _ => "application/octet-stream"
        };
    }
}
