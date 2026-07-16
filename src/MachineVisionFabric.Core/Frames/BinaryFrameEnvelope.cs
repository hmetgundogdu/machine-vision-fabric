using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Core.Frames;

public sealed class BinaryFrameEnvelope : IFrameEnvelope
{
    private readonly byte[] payload;

    public BinaryFrameEnvelope(
        string cameraId,
        int sequenceNumber,
        string fileName,
        byte[] payload,
        string contentType = "application/octet-stream",
        DateTime? timestampUtc = null,
        string? sourcePath = null)
    {
        CameraId = cameraId;
        SequenceNumber = sequenceNumber;
        FileName = fileName;
        this.payload = payload;
        ContentType = string.IsNullOrWhiteSpace(contentType) ? "application/octet-stream" : contentType;
        TimestampUtc = timestampUtc ?? DateTime.UtcNow;
        SourcePath = sourcePath;
        ContentLength = payload.LongLength;
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
        return ValueTask.FromResult<Stream>(new MemoryStream(payload, writable: false));
    }
}
