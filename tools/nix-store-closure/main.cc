#include "nix/store/globals.hh"
#include "nix/store/store-api.hh"
#include "nix/store/store-open.hh"

#include <nlohmann/json.hpp>

#include <algorithm>
#include <iostream>
#include <set>
#include <string>
#include <vector>

using json = nlohmann::json;

namespace {

constexpr std::size_t maximumRequestBytes = 1024 * 1024;
constexpr std::size_t maximumResponseBytes = 1024 * 1024;
constexpr std::size_t maximumDiagnosticBytes = 4096;
constexpr std::size_t maximumRoots = 4096;

std::string readRequest()
{
    std::string request;
    char buffer[4096];
    while (std::cin) {
        std::cin.read(buffer, sizeof(buffer));
        request.append(buffer, static_cast<std::size_t>(std::cin.gcount()));
        if (request.size() > maximumRequestBytes)
            throw std::runtime_error("closure request exceeds limit");
    }
    return request;
}

int run(const json & request)
{
    if (!request.is_object() || request.value("version", 0u) != 1
        || !request.contains("store_uri") || !request.at("store_uri").is_string()
        || request.at("store_uri").get<std::string>().empty()
        || !request.contains("roots") || !request.at("roots").is_array())
        throw std::runtime_error("invalid closure request");
    const auto & rootsJson = request.at("roots");
    if (rootsJson.size() > maximumRoots)
        throw std::runtime_error("closure root count exceeds limit");
    auto store = nix::openStore(request.at("store_uri").get<std::string>());
    nix::StorePathSet roots;
    for (const auto & value : rootsJson) {
        if (!value.is_string())
            throw std::runtime_error("invalid closure root");
        roots.insert(store->parseStorePath(value.get<std::string>()));
    }
    nix::StorePathSet closure;
    store->computeFSClosure(roots, closure, false, false, false);
    if (closure.size() > maximumRoots)
        throw std::runtime_error("closure path count exceeds limit");

    std::vector<std::string> printedPaths;
    printedPaths.reserve(closure.size());
    for (const auto & path : closure)
        printedPaths.push_back(store->printStorePath(path));
    std::sort(printedPaths.begin(), printedPaths.end());
    json paths = json::array();
    for (const auto & path : printedPaths)
        paths.push_back(path);
    json response = {{"version", 1}, {"paths", paths}};
    auto encoded = response.dump();
    if (encoded.size() > maximumResponseBytes)
        throw std::runtime_error("closure response exceeds limit");
    std::cout << encoded << '\n';
    return 0;
}

} // namespace

int main()
{
    try {
        nix::initLibStore();
        return run(json::parse(readRequest()));
    } catch (const std::exception & error) {
        std::string diagnostic = "closure helper failed: ";
        diagnostic += error.what();
        diagnostic += '\n';
        std::cerr.write(diagnostic.data(), std::min(diagnostic.size(), maximumDiagnosticBytes));
        return 1;
    }
}
