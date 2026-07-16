namespace MachineVisionFabric.Core.Abstractions;

public interface IFrameSourceSession : IAsyncDisposable
{
    int DeclaredCameraCount { get; }

    int? EstimatedFrameCount { get; }

    IAsyncEnumerable<IFrameEnvelope> ReadFramesAsync(CancellationToken cancellationToken);
}
