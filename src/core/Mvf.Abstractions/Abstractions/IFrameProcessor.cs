using Mvf.Graph.Processing;

namespace Mvf.Abstractions;

public interface IFrameProcessor
{
    Task<FrameProcessorDecision> EvaluateAsync(IFrameEnvelope frame, CancellationToken cancellationToken);
}
