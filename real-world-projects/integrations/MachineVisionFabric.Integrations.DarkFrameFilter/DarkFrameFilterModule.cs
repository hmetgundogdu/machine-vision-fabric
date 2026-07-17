using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Contracts.Processing;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;
using OpenCvSharp;

namespace MachineVisionFabric.Integrations.DarkFrameFilter;

/// <summary>
/// Integration module that filters out dark/underexposed frames using OpenCV.
///
/// Node category: compute
/// Input : frame
/// Output: frame (passed through only when mean brightness >= threshold)
/// </summary>
public sealed class DarkFrameFilterModule : FrameProcessorModuleBase<DarkFrameFilterOptions>
{
    protected override IntegrationModuleDescriptor BuildDescriptor() =>
        IntegrationModuleDescriptorBuilder.CreateProcessor<DarkFrameFilterOptions>(
            moduleId:       "mvf.realworld-dark-frame-filter",
            displayName:    "Dark Frame Filter",
            version:        "1.0.0",
            capabilityName: "dark-frame-filter",
            description:    "Rejects frames whose mean grayscale brightness falls below a configurable threshold.");

    protected override IFrameProcessor CreateProcessor(DarkFrameFilterOptions options) =>
        new DarkFrameProcessor(options);

    // ── Inner processor ──────────────────────────────────────────────────────

    private sealed class DarkFrameProcessor(DarkFrameFilterOptions options) : IFrameProcessor
    {
        public async Task<FrameProcessorDecision> EvaluateAsync(
            IFrameEnvelope frame,
            CancellationToken cancellationToken)
        {
            // Read frame bytes
            await using var src = await frame.OpenReadAsync(cancellationToken);
            using var ms = new MemoryStream();
            await src.CopyToAsync(ms, cancellationToken);
            var bytes = ms.ToArray();

            // Decode to grayscale
            using var img = Cv2.ImDecode(bytes, ImreadModes.Grayscale);

            if (img.Empty())
            {
                if (options.LogDecisions)
                    Console.WriteLine($"[DarkFrameFilter] decode failed — frame {(options.RejectOnDecodeFailure ? "rejected" : "accepted")}");

                return new FrameProcessorDecision(
                    !options.RejectOnDecodeFailure,
                    "dark-frame-filter",
                    "mean-grayscale-threshold",
                    DateTime.UtcNow,
                    "OpenCV could not decode frame");
            }

            var mean     = Cv2.Mean(img).Val0;
            var accepted = mean >= options.MinimumMeanBrightness;
            var details  = $"mean={mean:F1} threshold={options.MinimumMeanBrightness:F1}";

            if (options.LogDecisions)
                Console.WriteLine($"[DarkFrameFilter] {(accepted ? "ACCEPT" : "REJECT")}  {details}");

            return new FrameProcessorDecision(
                accepted,
                "dark-frame-filter",
                "mean-grayscale-threshold",
                DateTime.UtcNow,
                details);
        }
    }
}
