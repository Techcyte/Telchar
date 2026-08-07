#include "nix/store/globals.hh"
#include "nix/store/path-info.hh"
#include "nix/store/store-open.hh"
#include "nix/util/hash.hh"
#include "nix/util/serialise.hh"

#include <fcntl.h>
#include <nlohmann/json.hpp>
#include <unistd.h>

#include <cerrno>
#include <cstring>
#include <filesystem>
#include <iostream>
#include <set>
#include <string>

using json = nlohmann::json;

namespace {

constexpr std::size_t maximumRequestBytes = 64 * 1024;

std::string readRequest()
{
    std::string request;
    char buffer[4096];
    while (std::cin) {
        std::cin.read(buffer, sizeof(buffer));
        request.append(buffer, static_cast<std::size_t>(std::cin.gcount()));
        if (request.size() > maximumRequestBytes)
            throw std::runtime_error("promotion request exceeds limit");
    }
    return request;
}

int run()
{
    auto request = json::parse(readRequest());
    if (request.at("version").get<unsigned int>() != 1)
        throw std::runtime_error("unsupported promotion request version");

    auto storeUri = request.at("store_uri").get<std::string>();
    auto store = nix::openStore(storeUri);
    auto path = store->parseStorePath(request.at("path").get<std::string>());
    auto narHash = nix::Hash::parseAny(request.at("nar_hash_hex").get<std::string>(), nix::HashAlgorithm::SHA256);
    nix::ValidPathInfo info{path, {*store, narHash}};
    info.narSize = request.at("nar_size").get<std::uint64_t>();
    for (const auto & reference : request.at("references"))
        info.references.insert(store->parseStorePath(reference.get<std::string>()));
    if (!request.at("deriver").is_null()) {
        auto deriver = store->parseStorePath(request.at("deriver").get<std::string>());
        deriver.requireDerivation();
        info.deriver = std::move(deriver);
    }

    auto stagingDirectory = std::filesystem::weakly_canonical(request.at("staging_directory").get<std::string>());
    auto narPath = std::filesystem::weakly_canonical(request.at("nar_path").get<std::string>());
    if (narPath.parent_path() != stagingDirectory)
        throw std::runtime_error("staged NAR is outside configured staging directory");
    auto fd = open(narPath.c_str(), O_RDONLY | O_CLOEXEC | O_NOFOLLOW);
    if (fd == -1)
        throw std::runtime_error("cannot open staged NAR: " + std::string(std::strerror(errno)));
    nix::FdSource source{fd};
    try {
        store->addToStore(info, source, nix::NoRepair, nix::NoCheckSigs);
    } catch (...) {
        close(fd);
        throw;
    }
    close(fd);

    std::cout << R"({"version":1,"promoted":true})" << '\n';
    return 0;
}

} // namespace

int main()
{
    try {
        nix::initLibStore();
        return run();
    } catch (const std::exception & error) {
        std::cerr << error.what() << '\n';
        return 1;
    }
}
