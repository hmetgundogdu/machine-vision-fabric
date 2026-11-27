// Sample analyzer: the mean byte of a frame, as a classification ("dark" / "bright") and as a
// JSON value, with the number of frames it has seen as checkpointed state.
//
// On its first frame it also writes one line straight to the process's stdout, the way a native
// library might. Under protocol v2 the SDK has already moved the control channel off stdout, so the
// line lands in the engine's logs and the channel is untouched (protocol/worker-v2.md). Under v1
// the engine logs it as non-protocol output.

#include "mvf/sdk.hpp"

#include <cstdio>
#include <cstring>

using namespace mvf;

namespace {
uint64_t seen = 0;
}

int main() {
    ModuleHooks hooks;
    hooks.on_checkpoint = []() -> std::optional<Output> {
        return blob(&seen, sizeof seen);
    };
    hooks.on_restore = [](const Payload& state) {
        if (state.size == sizeof seen) std::memcpy(&seen, state.data, sizeof seen);
    };
    return run_analyzer("cpp.mean-analyzer",
        [](const Payload& payload, const json& /*meta*/) {
            if (seen++ == 0) {
                std::printf("native stdout: first frame\n");
                std::fflush(stdout);
                log("analyzing");
            }
            uint64_t sum = 0;
            for (size_t i = 0; i < payload.size; ++i) sum += payload.data[i];
            const double mean = payload.size ? static_cast<double>(sum) / payload.size : 0.0;
            AnalysisResult result;
            result.classification = Classification{mean < 128 ? "dark" : "bright", mean,
                                                   std::string("mean-byte"), std::nullopt};
            result.value = json{{"mean", mean}, {"frames", seen}};
            return result;
        },
        hooks);
}
