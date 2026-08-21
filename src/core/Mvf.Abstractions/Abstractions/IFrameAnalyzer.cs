namespace Mvf.Abstractions;

/// <summary>
/// Analyzes a frame and may emit multiple typed outputs in the same cycle: a derived frame, a
/// classification for control flow, and structured inference metadata for downstream logic.
/// </summary>
public interface IFrameAnalyzer
{
    Task<FrameAnalysisResult> AnalyzeAsync(IFrameEnvelope frame, CancellationToken cancellationToken);
}
