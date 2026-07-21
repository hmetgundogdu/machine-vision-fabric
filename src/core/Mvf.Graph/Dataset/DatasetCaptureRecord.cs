namespace Mvf.Graph.Dataset;

public sealed record DatasetCaptureRecord(
    string CameraId,
    int SequenceNumber,
    string SourcePath,
    string StoredImagePath,
    string MetadataPath,
    DateTime CapturedAtUtc,
    DateTime SourceTimestampUtc,
    string ContentType,
    long? ContentLength);
