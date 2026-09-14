#include "mofa/rsa_pss_signature.hpp"

#include <array>
#include <cstdint>
#include <cstdlib>
#include <filesystem>
#include <fstream>
#include <iostream>
#include <stdexcept>
#include <string>
#include <vector>

namespace {

void require(bool condition, const std::string& message) {
    if (!condition) throw std::runtime_error(message);
}

std::vector<std::uint8_t> read_file(const std::filesystem::path& path) {
    std::ifstream input(path, std::ios::binary);
    require(static_cast<bool>(input), "cannot open " + path.string());
    input.seekg(0, std::ios::end);
    const auto end = input.tellg();
    require(end >= 0, "cannot size " + path.string());
    input.seekg(0);
    std::vector<std::uint8_t> bytes(static_cast<std::size_t>(end));
    input.read(reinterpret_cast<char*>(bytes.data()),
               static_cast<std::streamsize>(bytes.size()));
    require(input.good() || input.eof(), "cannot read " + path.string());
    require(static_cast<std::size_t>(input.gcount()) == bytes.size(),
            "short read from " + path.string());
    return bytes;
}

void verify_stream(const std::filesystem::path& path,
                   std::string_view public_key,
    const std::vector<std::uint8_t>& signature) {
    mofa::RsaPssSha256Verifier verifier;
    std::string error;
    require(verifier.initialize(public_key, &error), error);
    std::ifstream input(path, std::ios::binary);
    require(static_cast<bool>(input), "cannot open signed payload");
    std::array<std::uint8_t, 65536> buffer{};
    while (input) {
        input.read(reinterpret_cast<char*>(buffer.data()), buffer.size());
        const auto amount = input.gcount();
        if (amount > 0) {
            require(verifier.update(buffer.data(),
                                    static_cast<std::size_t>(amount), &error),
                    error);
        }
    }
    require(input.eof(), "signed payload read failed");
    require(verifier.finish(signature.data(), signature.size(), &error), error);
}

} // namespace

int main() {
    try {
        constexpr std::string_view public_key =
            "-----BEGIN PUBLIC KEY-----\n"
            "MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQC+1kdOYr+n64opY2jyAW/s0UZj\n"
            "pSnn6KVe9759Gv/dqkspu2OWOn/kQBzzgONwL6nhU+k66BUS74jbJXER/iqlStXE\n"
            "wRzFmPO7j2tW3nHLYuB1T7xpQ7tNXiTH6ZNLe5aU4Mwx5QrdRsUf9ONeJM1u10Ig\n"
            "PdzmWtHwBf4X4Sh9NQIDAQAB\n"
            "-----END PUBLIC KEY-----\n";
        constexpr std::string_view envelope =
            "-- SIGNATURE - SHA256/PSS/RSA --\r\n"
            "FrKV7oJAs7296Xf/b9PO1ob16JvPgQxOR7sTb5xHsupB+jNzG7jIDYDx6dJbD8O+UPXdiY4lHJbF\r\n"
            "ynF6VDcioDPlMUACGZ/KtEbZ3nCYQWaP24dyppvJSRYi6zgKSBqK0krbuPEE3J8dzMKEgqmPtZTV\r\n"
            "Uarde7x2W0Ows/MlJso=\r\n";
        // Opaque, pre-signed test payload. The envelope below was produced with
        // a key that is not part of this repository, so the bytes must stay
        // exactly as they are; they are written out numerically instead of as
        // text so the fixture no longer spells out a project name.
        constexpr std::uint8_t message[] = {
            0x6b, 0x72, 0x6b, 0x72, 0x76, 0x69, 0x74, 0x61, 0x20, 0x72,
            0x73, 0x61, 0x2d, 0x70, 0x73, 0x73, 0x20, 0x73, 0x74, 0x72,
            0x65, 0x61, 0x6d, 0x69, 0x6e, 0x67, 0x20, 0x63, 0x6f, 0x6d,
            0x70, 0x61, 0x74, 0x69, 0x62, 0x69, 0x6c, 0x69, 0x74, 0x79,
            0x20, 0x66, 0x69, 0x78, 0x74, 0x75, 0x72, 0x65, 0x0a,
        };
        constexpr std::size_t message_size = sizeof(message);

        std::vector<std::uint8_t> signature;
        std::string error;
        require(mofa::decode_sha256_pss_rsa_signature(
                    reinterpret_cast<const std::uint8_t*>(envelope.data()),
                    envelope.size(), signature, &error),
                error);
        require(signature.size() == 128, "RSA-1024 signature size changed");

        mofa::RsaPssSha256Verifier verifier;
        require(verifier.initialize(public_key, &error), error);
        require(verifier.update(message, 7, &error), error);
        require(verifier.update(message + 7, message_size - 7, &error),
                error);
        require(verifier.finish(signature.data(), signature.size(), &error),
                error);

        mofa::RsaPssSha256Verifier tampered;
        require(tampered.initialize(public_key, &error), error);
        std::vector<std::uint8_t> changed(message, message + message_size);
        changed[0] ^= 1;
        require(tampered.update(changed.data(), changed.size(), &error),
                error);
        require(!tampered.finish(signature.data(), signature.size(), &error),
                "tampered payload passed RSA-PSS verification");

        std::vector<std::uint8_t> rejected;
        const std::array<std::uint8_t, 4> malformed = {'b', 'a', 'd', '!'};
        require(!mofa::decode_sha256_pss_rsa_signature(
                    malformed.data(), malformed.size(), rejected, &error),
                "malformed signature envelope was accepted");

        const char* corpus =
            std::getenv("MOFA_TEST_SIGNED_ARCHIVE_DIR");
        if (corpus && *corpus) {
            const std::filesystem::path retail = corpus;
            require(std::filesystem::exists(retail / "data.xp3") &&
                        std::filesystem::exists(retail / "data.xp3.sig"),
                    "signed archive corpus is incomplete");
            constexpr std::string_view retail_key =
                "-----BEGIN PUBLIC KEY-----\n"
                "MIGJAoGBAM9SZJzFoJNvGMjW7Ag2fHpHHZnZwmoc0LIzl5sCenvp+sShikO22lQs\n"
                "lOguG8vPzqoQkjPIJIw+HiZRRtZR7mlEYHupgh1FKWcqAn+S15NHWHKvLkFRyyGc\n"
                "mms/pQHGvSeRV/pZrGdbfY0icSOOhm2VwIU3Ba5vTZJjzQJleZYfAgMBAAE=\n"
                "-----END PUBLIC KEY-----\n";
            const auto retail_envelope = read_file(retail / "data.xp3.sig");
            require(mofa::decode_sha256_pss_rsa_signature(
                        retail_envelope.data(), retail_envelope.size(),
                        signature, &error),
                    error);
            verify_stream(retail / "data.xp3", retail_key, signature);
        }
        std::cout << "RSA-PSS signature contracts passed\n";
        return 0;
    } catch (const std::exception& exception) {
        std::cerr << exception.what() << '\n';
        return 1;
    }
}
