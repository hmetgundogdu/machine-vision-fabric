using MachineVisionFabric.Contracts.Processing;

namespace MachineVisionFabric.Core.Abstractions;

/// <summary>
/// Classifies a frame's content into a routing label plus optional measurement.
/// The perception → control bridge: consumes a data frame, produces a control decision.
/// </summary>
public interface IFrameClassifier
{
    Task<FrameClassification> ClassifyAsync(IFrameEnvelope frame, CancellationToken cancellationToken);
}
