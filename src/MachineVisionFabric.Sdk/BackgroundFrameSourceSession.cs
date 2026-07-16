using System.Runtime.CompilerServices;
using System.Threading.Channels;
using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Sdk;

public abstract class BackgroundFrameSourceSession : IFrameSourceSession
{
    private readonly Channel<IFrameEnvelope> channel;
    private readonly CancellationTokenSource disposeCts = new();
    private Task? producerTask;

    protected BackgroundFrameSourceSession(int declaredCameraCount, int? estimatedFrameCount, int boundedCapacity = 8)
    {
        DeclaredCameraCount = Math.Max(1, declaredCameraCount);
        EstimatedFrameCount = estimatedFrameCount;

        channel = Channel.CreateBounded<IFrameEnvelope>(new BoundedChannelOptions(Math.Max(1, boundedCapacity))
        {
            SingleReader = true,
            SingleWriter = true,
            FullMode = BoundedChannelFullMode.Wait
        });
    }

    public int DeclaredCameraCount { get; }

    public int? EstimatedFrameCount { get; }

    public async IAsyncEnumerable<IFrameEnvelope> ReadFramesAsync([EnumeratorCancellation] CancellationToken cancellationToken)
    {
        await foreach (var frame in channel.Reader.ReadAllAsync(cancellationToken))
        {
            yield return frame;
        }
    }

    public async ValueTask DisposeAsync()
    {
        disposeCts.Cancel();

        if (producerTask is not null)
        {
            try
            {
                await producerTask;
            }
            catch (OperationCanceledException)
            {
            }
        }

        disposeCts.Dispose();
    }

    protected void StartBackgroundProducer(Func<CancellationToken, Task> producer)
    {
        if (producerTask is not null)
        {
            throw new InvalidOperationException("Background producer has already been started.");
        }

        producerTask = Task.Run(async () =>
        {
            try
            {
                await producer(disposeCts.Token);
                channel.Writer.TryComplete();
            }
            catch (OperationCanceledException)
            {
                channel.Writer.TryComplete();
                throw;
            }
            catch (Exception ex)
            {
                channel.Writer.TryComplete(ex);
            }
        }, disposeCts.Token);
    }

    protected ValueTask PublishAsync(IFrameEnvelope frame, CancellationToken cancellationToken)
    {
        return channel.Writer.WriteAsync(frame, cancellationToken);
    }
}
