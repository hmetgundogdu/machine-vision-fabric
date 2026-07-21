using System.Text.Json;
using Mvf.Graph.Dataset;
using Mvf.Abstractions;

namespace MachineVisionFabric.Integrations.DatasetWriter;

/// <summary>
/// Frame sink that persists each frame as an image file plus a per-frame JSON
/// metadata record under a timestamped session directory:
/// <code>
/// {sessionRoot}/
///   images/   000001-{cameraId}-seq{seqN}.{ext}
///   metadata/ 000001-{cameraId}-seq{seqN}.json
///   session.json   (written on flush)
/// </code>
/// </summary>
internal sealed class DatasetWriterSink : IFrameSink
{
    private static readonly JsonSerializerOptions JsonOptions =
        new(JsonSerializerDefaults.Web) { WriteIndented = true };

    private static readonly string[] ImageExtensions =
        [".jpg", ".jpeg", ".png", ".bmp", ".tif", ".tiff"];

    private readonly string _sessionRoot;
    private readonly bool _includeSourcePath;
    private readonly string _imagesRoot;
    private readonly string _metadataRoot;
    private readonly List<DatasetCaptureRecord> _records = [];
    private int _globalSequence;

    public DatasetWriterSink(string sessionRoot, bool includeSourcePath)
    {
        _sessionRoot = sessionRoot;
        _includeSourcePath = includeSourcePath;
        _imagesRoot = Path.Combine(sessionRoot, "images");
        _metadataRoot = Path.Combine(sessionRoot, "metadata");
        Directory.CreateDirectory(_imagesRoot);
        Directory.CreateDirectory(_metadataRoot);
    }

    public async Task WriteAsync(IFrameEnvelope frame, CancellationToken cancellationToken)
    {
        _globalSequence++;
        var extension = ResolveExtension(frame.FileName, frame.ContentType);
        var storedFileName = $"{_globalSequence:000000}-{frame.CameraId}-seq{frame.SequenceNumber:0000}{extension}";
        var storedImagePath = Path.Combine(_imagesRoot, storedFileName);

        await using (var source = await frame.OpenReadAsync(cancellationToken))
        await using (var dest = File.Create(storedImagePath))
        {
            await source.CopyToAsync(dest, cancellationToken);
        }

        var metadataFileName = Path.GetFileNameWithoutExtension(storedFileName) + ".json";
        var metadataPath = Path.Combine(_metadataRoot, metadataFileName);

        var record = new DatasetCaptureRecord(
            CameraId: frame.CameraId,
            SequenceNumber: frame.SequenceNumber,
            SourcePath: _includeSourcePath ? frame.SourcePath ?? string.Empty : string.Empty,
            StoredImagePath: storedImagePath,
            MetadataPath: metadataPath,
            CapturedAtUtc: DateTime.UtcNow,
            SourceTimestampUtc: frame.TimestampUtc,
            ContentType: frame.ContentType,
            ContentLength: frame.ContentLength);

        await using var metaStream = File.Create(metadataPath);
        await JsonSerializer.SerializeAsync(metaStream, record, JsonOptions, cancellationToken);

        _records.Add(record);
    }

    public async Task FlushAsync(CancellationToken cancellationToken)
    {
        if (_records.Count == 0) return;

        var summary = new DatasetSessionSummary(
            SessionRoot: _sessionRoot,
            FrameCount: _records.Count,
            FinalizedAtUtc: DateTime.UtcNow,
            Records: _records);

        var sessionJsonPath = Path.Combine(_sessionRoot, "session.json");
        await using var stream = File.Create(sessionJsonPath);
        await JsonSerializer.SerializeAsync(stream, summary, JsonOptions, cancellationToken);
    }

    public ValueTask DisposeAsync() => ValueTask.CompletedTask;

    private static string ResolveExtension(string fileName, string contentType)
    {
        var ext = Path.GetExtension(fileName);
        if (!string.IsNullOrWhiteSpace(ext)
            && ImageExtensions.Contains(ext, StringComparer.OrdinalIgnoreCase))
        {
            return ext;
        }

        return contentType.ToLowerInvariant() switch
        {
            "image/jpeg" => ".jpg",
            "image/png" => ".png",
            "image/bmp" => ".bmp",
            "image/tiff" => ".tif",
            _ => ".bin"
        };
    }
}
