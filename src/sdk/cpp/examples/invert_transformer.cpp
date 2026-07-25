// Sample polyglot transformer: inverts every byte of a frame (b -> 255 - b).
//
// The C++ twin of modules/py-invert-transformer/transformer.py. The input arrives as a
// zero-copy Payload over shared memory and the output is written straight back into the
// arena (no base64, no bytes on the pipe). Build it by linking libmvf_sdk; the engine
// launches the resulting executable as an out-of-process module.

#include "mvf/sdk.hpp"

using namespace mvf;

int main() {
    return run_processor("cpp.invert-transformer",
        [](const Payload& payload, const json& /*meta*/) -> std::optional<Output> {
            std::string inverted(payload.size, '\0');
            for (size_t i = 0; i < payload.size; ++i)
                inverted[i] = static_cast<char>(255 - payload.data[i]);
            return blob(std::move(inverted));
        });
}
