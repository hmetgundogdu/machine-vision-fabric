using System.Text.Json;
using MachineVisionFabric.Contracts.Control;
using MachineVisionFabric.Contracts.Dataset;
using MachineVisionFabric.Contracts.Packages;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Core.Frames;

namespace MachineVisionFabric.Storage;

public sealed class DatasetCollector : IDatasetCollector
{
    private static readonly JsonSerializerOptions JsonOptions = new(JsonSerializerDefaults.Web)
    {
        WriteIndented = true
    };

    public async Task<DatasetCollectionResult> CollectAsync(
        string sessionRoot,
        FabricProfileManifest manifest,
        int declaredCameraCount,
        IProductPresenceGate productPresenceGate,
        IFrameProcessor? frameProcessor,
        IFrameSourceSession frameSourceSession,
        CancellationToken cancellationToken)
    {
        var imagesRoot = Path.Combine(sessionRoot, "images");
        var metadataRoot = Path.Combine(sessionRoot, "metadata");

        Directory.CreateDirectory(imagesRoot);
        Directory.CreateDirectory(metadataRoot);

        var writeContext = new CaptureWriteContext(imagesRoot, metadataRoot, manifest.CapturePolicy);
        var productPresenceDecision = IsTriggerWindowMode(manifest.CapturePolicy)
            ? await CollectTriggerWindowAsync(writeContext, productPresenceGate, frameProcessor, frameSourceSession, declaredCameraCount, cancellationToken)
            : await CollectFullStreamAsync(writeContext, productPresenceGate, frameProcessor, frameSourceSession, declaredCameraCount, cancellationToken);

        return await FinalizeSessionAsync(
            sessionRoot,
            manifest,
            declaredCameraCount,
            productPresenceDecision,
            manifest.CapturePolicy,
            writeContext.Records,
            cancellationToken);
    }

    private static async Task<ProductPresenceDecision> CollectFullStreamAsync(
        CaptureWriteContext writeContext,
        IProductPresenceGate productPresenceGate,
        IFrameProcessor? frameProcessor,
        IFrameSourceSession frameSourceSession,
        int declaredCameraCount,
        CancellationToken cancellationToken)
    {
        var capturePolicy = writeContext.CapturePolicy;
        var productPresenceDecision = await productPresenceGate.EvaluateAsync(cancellationToken);

        if (capturePolicy.RequireProductPresent && !productPresenceDecision.ProductPresent)
        {
            return productPresenceDecision;
        }

        var maxFramesPerCamera = capturePolicy.MaxFramesPerCamera is > 0 ? capturePolicy.MaxFramesPerCamera : null;
        var capturedFramesPerCamera = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase);

        await foreach (var frame in frameSourceSession.ReadFramesAsync(cancellationToken))
        {
            cancellationToken.ThrowIfCancellationRequested();

            if (maxFramesPerCamera is int maxFrameBudget)
            {
                if (capturedFramesPerCamera.TryGetValue(frame.CameraId, out var cameraCount) && cameraCount >= maxFrameBudget)
                {
                    if (HasReachedCaptureBudget(capturedFramesPerCamera, declaredCameraCount, maxFrameBudget))
                    {
                        break;
                    }

                    continue;
                }
            }

            if (!await ShouldPersistFrameAsync(frameProcessor, frame, cancellationToken))
            {
                continue;
            }

            await WriteFrameAsync(writeContext, frame, cancellationToken);
            capturedFramesPerCamera[frame.CameraId] = capturedFramesPerCamera.GetValueOrDefault(frame.CameraId) + 1;

            if (maxFramesPerCamera is int maxFrames && HasReachedCaptureBudget(capturedFramesPerCamera, declaredCameraCount, maxFrames))
            {
                break;
            }
        }

        return productPresenceDecision;
    }

    private static async Task<ProductPresenceDecision> CollectTriggerWindowAsync(
        CaptureWriteContext writeContext,
        IProductPresenceGate productPresenceGate,
        IFrameProcessor? frameProcessor,
        IFrameSourceSession frameSourceSession,
        int declaredCameraCount,
        CancellationToken cancellationToken)
    {
        var capturePolicy = writeContext.CapturePolicy;
        var preTriggerFramesPerCamera = Math.Max(0, capturePolicy.PreTriggerFramesPerCamera);
        var postTriggerFramesPerCamera = Math.Max(0, capturePolicy.PostTriggerFramesPerCamera);
        var gateEvaluationIntervalFrames = Math.Max(1, capturePolicy.GateEvaluationIntervalFrames);
        var bufferedFrames = new Dictionary<string, Queue<BufferedFrameSnapshot>>(StringComparer.OrdinalIgnoreCase);
        var capturedPostFramesPerCamera = new Dictionary<string, int>(StringComparer.OrdinalIgnoreCase);
        var latestDecision = new ProductPresenceDecision(
            false,
            "trigger-window",
            "trigger-window",
            DateTime.UtcNow,
            "Gate was not evaluated.");
        ProductPresenceDecision? triggerDecision = null;
        var triggerFired = false;
        var arrivalIndex = 0L;
        var observedFrameCount = 0;

        await foreach (var frame in frameSourceSession.ReadFramesAsync(cancellationToken))
        {
            cancellationToken.ThrowIfCancellationRequested();

            var snapshot = await SnapshotFrameAsync(frame, cancellationToken);
            arrivalIndex++;

            if (!triggerFired)
            {
                var cameraQueue = GetOrCreateQueue(bufferedFrames, snapshot.CameraId);
                cameraQueue.Enqueue(new BufferedFrameSnapshot(arrivalIndex, snapshot));
                TrimQueue(cameraQueue, preTriggerFramesPerCamera + 1);

                observedFrameCount++;
                if (observedFrameCount == 1 || observedFrameCount % gateEvaluationIntervalFrames == 0)
                {
                    latestDecision = await productPresenceGate.EvaluateAsync(cancellationToken);
                }

                if (!latestDecision.ProductPresent)
                {
                    continue;
                }

                triggerFired = true;
                triggerDecision = latestDecision;

                var framesToPersist = bufferedFrames.Values
                    .SelectMany(queue => queue)
                    .OrderBy(snapshotItem => snapshotItem.ArrivalIndex)
                    .Select(snapshotItem => snapshotItem.Frame)
                    .ToArray();

                foreach (var bufferedFrame in framesToPersist)
                {
                    if (!await ShouldPersistFrameAsync(frameProcessor, bufferedFrame, cancellationToken))
                    {
                        continue;
                    }

                    await WriteFrameAsync(writeContext, bufferedFrame, cancellationToken);
                }

                if (postTriggerFramesPerCamera == 0)
                {
                    break;
                }

                continue;
            }

            if (capturedPostFramesPerCamera.TryGetValue(snapshot.CameraId, out var currentCount) && currentCount >= postTriggerFramesPerCamera)
            {
                if (HasReachedCaptureBudget(capturedPostFramesPerCamera, declaredCameraCount, postTriggerFramesPerCamera))
                {
                    break;
                }

                continue;
            }

            if (!await ShouldPersistFrameAsync(frameProcessor, snapshot, cancellationToken))
            {
                continue;
            }

            await WriteFrameAsync(writeContext, snapshot, cancellationToken);
            capturedPostFramesPerCamera[snapshot.CameraId] = capturedPostFramesPerCamera.GetValueOrDefault(snapshot.CameraId) + 1;

            if (HasReachedCaptureBudget(capturedPostFramesPerCamera, declaredCameraCount, postTriggerFramesPerCamera))
            {
                break;
            }
        }

        if (triggerDecision is not null)
        {
            return triggerDecision;
        }

        if (latestDecision.Source == "trigger-window" && latestDecision.StationId == "trigger-window")
        {
            latestDecision = await productPresenceGate.EvaluateAsync(cancellationToken);
        }

        return latestDecision;
    }

    private static bool HasReachedCaptureBudget(
        IReadOnlyDictionary<string, int> capturedFramesPerCamera,
        int declaredCameraCount,
        int maxFramesPerCamera)
    {
        return capturedFramesPerCamera.Count >= Math.Max(1, declaredCameraCount)
            && capturedFramesPerCamera.Values.All(count => count >= maxFramesPerCamera);
    }

    private static string ResolveStoredExtension(string fileName, string contentType)
    {
        var extension = Path.GetExtension(fileName);
        if (!string.IsNullOrWhiteSpace(extension))
        {
            return extension;
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

    private static async Task WriteFrameAsync(
        CaptureWriteContext writeContext,
        IFrameEnvelope frame,
        CancellationToken cancellationToken)
    {
        writeContext.GlobalSequence++;
        var sourceExtension = ResolveStoredExtension(frame.FileName, frame.ContentType);
        var storedFileName = $"{writeContext.GlobalSequence:000000}-{frame.CameraId}-seq{frame.SequenceNumber:0000}{sourceExtension}";
        var storedImagePath = Path.Combine(writeContext.ImagesRoot, storedFileName);

        await using (var sourceStream = await frame.OpenReadAsync(cancellationToken))
        await using (var destinationStream = File.Create(storedImagePath))
        {
            await sourceStream.CopyToAsync(destinationStream, cancellationToken);
        }

        var metadataFileName = Path.GetFileNameWithoutExtension(storedFileName) + ".json";
        var metadataPath = Path.Combine(writeContext.MetadataRoot, metadataFileName);

        var record = new DatasetCaptureRecord(
            frame.CameraId,
            frame.SequenceNumber,
            writeContext.CapturePolicy.IncludeSourcePathInMetadata ? frame.SourcePath ?? string.Empty : string.Empty,
            storedImagePath,
            metadataPath,
            DateTime.UtcNow,
            frame.TimestampUtc,
            frame.ContentType,
            frame.ContentLength);

        await using (var metadataStream = File.Create(metadataPath))
        {
            await JsonSerializer.SerializeAsync(metadataStream, record, JsonOptions, cancellationToken);
        }

        writeContext.Records.Add(record);
    }

    private static async Task<IFrameEnvelope> SnapshotFrameAsync(IFrameEnvelope frame, CancellationToken cancellationToken)
    {
        await using var sourceStream = await frame.OpenReadAsync(cancellationToken);
        using var memoryStream = new MemoryStream();
        await sourceStream.CopyToAsync(memoryStream, cancellationToken);

        return new BinaryFrameEnvelope(
            frame.CameraId,
            frame.SequenceNumber,
            frame.FileName,
            memoryStream.ToArray(),
            frame.ContentType,
            frame.TimestampUtc,
            frame.SourcePath);
    }

    private static async Task<bool> ShouldPersistFrameAsync(
        IFrameProcessor? frameProcessor,
        IFrameEnvelope frame,
        CancellationToken cancellationToken)
    {
        if (frameProcessor is null)
        {
            return true;
        }

        var decision = await frameProcessor.EvaluateAsync(frame, cancellationToken);
        return decision.Accepted;
    }

    private static bool IsTriggerWindowMode(DatasetCapturePolicy capturePolicy)
    {
        return string.Equals(capturePolicy.Mode, "trigger-window", StringComparison.OrdinalIgnoreCase);
    }

    private static Queue<BufferedFrameSnapshot> GetOrCreateQueue(
        IDictionary<string, Queue<BufferedFrameSnapshot>> bufferedFrames,
        string cameraId)
    {
        if (!bufferedFrames.TryGetValue(cameraId, out var queue))
        {
            queue = new Queue<BufferedFrameSnapshot>();
            bufferedFrames[cameraId] = queue;
        }

        return queue;
    }

    private static void TrimQueue(Queue<BufferedFrameSnapshot> queue, int capacity)
    {
        while (queue.Count > Math.Max(1, capacity))
        {
            queue.Dequeue();
        }
    }

    private static async Task<DatasetCollectionResult> FinalizeSessionAsync(
        string sessionRoot,
        FabricProfileManifest manifest,
        int declaredCameraCount,
        ProductPresenceDecision productPresenceDecision,
        DatasetCapturePolicy capturePolicy,
        IReadOnlyList<DatasetCaptureRecord> records,
        CancellationToken cancellationToken)
    {
        var sessionMetadata = new DatasetSessionMetadata
        {
            PackageName = manifest.Name,
            SessionRoot = sessionRoot,
            CreatedAtUtc = DateTime.UtcNow,
            CapturedFrameCount = records.Count,
            DeclaredCameraCount = declaredCameraCount,
            Scenario = manifest.Scenario,
            CapturePolicy = capturePolicy,
            ProductPresenceDecision = productPresenceDecision,
            Records = records
        };

        var sessionMetadataPath = Path.Combine(sessionRoot, "session.json");
        await using (var stream = File.Create(sessionMetadataPath))
        {
            await JsonSerializer.SerializeAsync(stream, sessionMetadata, JsonOptions, cancellationToken);
        }

        return new DatasetCollectionResult(records.Count, sessionMetadataPath, records, productPresenceDecision);
    }

    private sealed class CaptureWriteContext(string imagesRoot, string metadataRoot, DatasetCapturePolicy capturePolicy)
    {
        public string ImagesRoot { get; } = imagesRoot;

        public string MetadataRoot { get; } = metadataRoot;

        public DatasetCapturePolicy CapturePolicy { get; } = capturePolicy;

        public List<DatasetCaptureRecord> Records { get; } = [];

        public int GlobalSequence { get; set; }
    }

    private sealed record BufferedFrameSnapshot(long ArrivalIndex, IFrameEnvelope Frame);
}
