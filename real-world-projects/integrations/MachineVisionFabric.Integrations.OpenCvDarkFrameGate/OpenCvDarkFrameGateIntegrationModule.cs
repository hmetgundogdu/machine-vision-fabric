using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Contracts.Processing;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;
using OpenCvSharp;

namespace MachineVisionFabric.Integrations.OpenCvDarkFrameGate;

public sealed class OpenCvDarkFrameGateIntegrationModule : FrameProcessorModuleBase<OpenCvDarkFrameGateOptions>
{
    protected override IntegrationModuleDescriptor BuildDescriptor()
    {
        return IntegrationModuleDescriptorBuilder.CreateProcessor<OpenCvDarkFrameGateOptions>(
            "mvf.opencv-dark-frame-gate",
            "OpenCV Dark Frame Gate",
            "0.1.0",
            "dark-frame-processor",
            "OpenCV-based frame processor that rejects very dark frames before dataset persistence.");
    }

    protected override IFrameProcessor CreateProcessor(OpenCvDarkFrameGateOptions options)
    {
        return new OpenCvDarkFrameProcessor(options);
    }

    private sealed class OpenCvDarkFrameProcessor(OpenCvDarkFrameGateOptions options) : IFrameProcessor
    {
        public async Task<FrameProcessorDecision> EvaluateAsync(IFrameEnvelope frame, CancellationToken cancellationToken)
        {
            await using var sourceStream = await frame.OpenReadAsync(cancellationToken);
            using var memoryStream = new MemoryStream();
            await sourceStream.CopyToAsync(memoryStream, cancellationToken);

            var buffer = memoryStream.ToArray();
            using var image = Cv2.ImDecode(buffer, ImreadModes.Grayscale);
            if (image.Empty())
            {
                return new FrameProcessorDecision(
                    !options.RejectOnDecodeFailure,
                    options.SourceName,
                    options.StrategyName,
                    DateTime.UtcNow,
                    "OpenCV could not decode the frame.");
            }

            var mean = Cv2.Mean(image).Val0;
            var accepted = mean >= options.MinimumMeanBrightness;
            var details = $"meanBrightness={mean:F2}; minimumMeanBrightness={options.MinimumMeanBrightness:F2}";

            return new FrameProcessorDecision(
                accepted,
                options.SourceName,
                options.StrategyName,
                DateTime.UtcNow,
                details);
        }
    }
}
