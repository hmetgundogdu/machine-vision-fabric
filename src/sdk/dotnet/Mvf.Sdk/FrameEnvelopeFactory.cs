using Mvf.Abstractions;
using Mvf.Abstractions.Frames;

namespace Mvf.Sdk;

public static class FrameEnvelopeFactory
{
    public static IFrameEnvelope FromFile(
        string cameraId,
        int sequenceNumber,
        string sourcePath,
        string? fileName = null,
        DateTime? timestampUtc = null)
    {
        return new FileFrameEnvelope(cameraId, sequenceNumber, sourcePath, fileName, timestampUtc);
    }

    public static IFrameEnvelope FromBytes(
        string cameraId,
        int sequenceNumber,
        string fileName,
        byte[] payload,
        string contentType = "application/octet-stream",
        DateTime? timestampUtc = null,
        string? sourcePath = null)
    {
        return new BinaryFrameEnvelope(cameraId, sequenceNumber, fileName, payload, contentType, timestampUtc, sourcePath);
    }
}
