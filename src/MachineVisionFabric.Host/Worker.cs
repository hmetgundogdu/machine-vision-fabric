using MachineVisionFabric.Core.Abstractions;

namespace MachineVisionFabric.Host;

public sealed class Worker(
    IHeadlessRuntimeBootstrapper bootstrapper,
    IHostApplicationLifetime hostApplicationLifetime,
    ILogger<Worker> logger) : BackgroundService
{
    protected override async Task ExecuteAsync(CancellationToken stoppingToken)
    {
        try
        {
            var report = await bootstrapper.BootstrapAsync(stoppingToken);

            logger.LogInformation(
                "Headless dataset-first MVP bootstrap completed. PackageRoot={PackageRoot}; SessionRoot={SessionRoot}; SessionCreated={SessionCreated}; ExpectedFrames={ExpectedFrames}; DeclaredCameraCount={DeclaredCameraCount}; ProductPresent={ProductPresent}; ProductPresenceSource={ProductPresenceSource}; ProductPresenceStrategy={ProductPresenceStrategy}; FrameSourceSource={FrameSourceSource}; FrameSourceStrategy={FrameSourceStrategy}; CapturedFrames={CapturedFrames}; SessionMetadataPath={SessionMetadataPath}",
                report.PackageRoot,
                report.DatasetSessionRoot,
                report.SessionCreated,
                report.ExpectedFrameCount,
                report.DeclaredCameraCount,
                report.ProductPresent,
                report.ProductPresenceSource,
                report.ProductPresenceStrategy,
                report.FrameSourceSource,
                report.FrameSourceStrategy,
                report.CapturedFrameCount,
                report.SessionMetadataPath);
        }
        catch (Exception ex)
        {
            logger.LogError(ex, "Headless dataset-first MVP bootstrap failed.");
        }
        finally
        {
            hostApplicationLifetime.StopApplication();
        }
    }
}
