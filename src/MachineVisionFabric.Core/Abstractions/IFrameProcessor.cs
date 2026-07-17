using MachineVisionFabric.Contracts.Processing;

namespace MachineVisionFabric.Core.Abstractions;

public interface IFrameProcessor
{
    Task<FrameProcessorDecision> EvaluateAsync(IFrameEnvelope frame, CancellationToken cancellationToken);
}
