namespace MachineVisionFabric.Core.Abstractions;

public interface IFrameEnvelope
{
    string CameraId { get; }

    int SequenceNumber { get; }

    string FileName { get; }

    string? SourcePath { get; }

    DateTime TimestampUtc { get; }

    string ContentType { get; }

    long? ContentLength { get; }

    ValueTask<Stream> OpenReadAsync(CancellationToken cancellationToken);
}
