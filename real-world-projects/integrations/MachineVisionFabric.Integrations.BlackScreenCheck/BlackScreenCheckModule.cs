using MachineVisionFabric.Contracts.Integrations;
using MachineVisionFabric.Contracts.Processing;
using MachineVisionFabric.Core.Abstractions;
using MachineVisionFabric.Sdk;
using OpenCvSharp;

namespace MachineVisionFabric.Integrations.BlackScreenCheck;

/// <summary>
/// Compute module that checks whether a frame is "very black" using OpenCV mean
/// grayscale brightness. It PASSES only the black frames (and logs a warning for
/// each), so a downstream branch can save/flag them. Bright frames are dropped
/// from this branch.
///
/// Node category: compute. Input: frame. Output: frame (only when black).
/// Pair it with a fork so the same source frame still reaches the main writer,
/// while this branch feeds a separate "dark frames" writer.
/// </summary>
public sealed class BlackScreenCheckModule : FrameProcessorModuleBase<BlackScreenCheckOptions>
{
    protected override IntegrationModuleDescriptor BuildDescriptor() =>
        IntegrationModuleDescriptorBuilder.CreateProcessor<BlackScreenCheckOptions>(
            moduleId:       "mvf.black-screen-check",
            displayName:    "Black Screen Check",
            version:        "1.0.0",
            capabilityName: "black-screen-check",
            description:    "Passes through frames whose mean grayscale brightness is at or below a threshold (very black), logging a warning for each.");

    protected override IFrameProcessor CreateProcessor(BlackScreenCheckOptions options) =>
        new BlackScreenProcessor(options);

    private sealed class BlackScreenProcessor(BlackScreenCheckOptions options) : IFrameProcessor
    {
        public async Task<FrameProcessorDecision> EvaluateAsync(
            IFrameEnvelope frame,
            CancellationToken cancellationToken)
        {
            await using var src = await frame.OpenReadAsync(cancellationToken);
            using var ms = new MemoryStream();
            await src.CopyToAsync(ms, cancellationToken);
            var bytes = ms.ToArray();

            using var img = Cv2.ImDecode(bytes, ImreadModes.Grayscale);

            if (img.Empty())
            {
                if (options.LogWarnings)
                    Console.WriteLine(
                        $"[BlackScreenCheck] decode failed - treated as {(options.TreatDecodeFailureAsBlack ? "BLACK" : "not black")}");

                return new FrameProcessorDecision(
                    options.TreatDecodeFailureAsBlack,
                    "black-screen-check",
                    "mean-grayscale-threshold",
                    DateTime.UtcNow,
                    "OpenCV could not decode frame");
            }

            var mean    = Cv2.Mean(img).Val0;
            var isBlack = mean <= options.DarknessThreshold;
            var details = $"mean={mean:F1} threshold={options.DarknessThreshold:F1}";

            if (isBlack && options.LogWarnings)
                Console.WriteLine($"[BlackScreenCheck] [WARN] BLACK screen detected - {details}");

            return new FrameProcessorDecision(
                isBlack,
                "black-screen-check",
                "mean-grayscale-threshold",
                DateTime.UtcNow,
                details);
        }
    }
}
