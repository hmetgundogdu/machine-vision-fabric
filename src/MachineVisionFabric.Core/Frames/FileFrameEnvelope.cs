using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Core.Frames;

public sealed class FileFrameEnvelope : IFrameEnvelope
{
    public FileFrameEnvelope(
        string cameraId,
        int sequenceNumber,
        string sourcePath,
        string? fileName = null,
        DateTime? timestampUtc = null)
    {
        CameraId = cameraId;
        SequenceNumber = sequenceNumber;
        SourcePath = Path.GetFullPath(sourcePath);
        FileName = string.IsNullOrWhiteSpace(fileName) ? Path.GetFileName(SourcePath) : fileName;
        TimestampUtc = timestampUtc ?? DateTime.UtcNow;
        ContentType = ResolveContentType(FileName);

        var fileInfo = new FileInfo(SourcePath);
        ContentLength = fileInfo.Exists ? fileInfo.Length : null;
    }

    public string CameraId { get; }

    public int SequenceNumber { get; }

    public string FileName { get; }

    public string? SourcePath { get; }

    public DateTime TimestampUtc { get; }

    public string ContentType { get; }

    public long? ContentLength { get; }

    public ValueTask<Stream> OpenReadAsync(CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        return ValueTask.FromResult<Stream>(File.OpenRead(SourcePath!));
    }

    private static string ResolveContentType(string fileName)
    {
        return Path.GetExtension(fileName).ToLowerInvariant() switch
        {
            ".jpg" or ".jpeg" => "image/jpeg",
            ".png" => "image/png",
            ".bmp" => "image/bmp",
            ".tif" or ".tiff" => "image/tiff",
            _ => "application/octet-stream"
        };
    }
}
