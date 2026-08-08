#include "nix/store/globals.hh"
#include "nix/store/indirect-root-store.hh"
#include "nix/store/store-api.hh"
#include "nix/store/store-open.hh"

#include <nlohmann/json.hpp>

#include <algorithm>
#include <filesystem>
#include <iostream>
#include <string>
#include <vector>

using json = nlohmann::json;

namespace {

constexpr std::size_t maximumRequestBytes = 1024 * 1024;
constexpr std::size_t maximumResponseBytes = 1024 * 1024;
constexpr std::size_t maximumDiagnosticBytes = 4096;
constexpr std::size_t maximumEntries = 4097;

struct Entry
{
    std::string leaseId;
    std::string storePath;
    std::filesystem::path rootPath;
};

std::string readRequest()
{
    std::string request;
    char buffer[4096];
    while (std::cin) {
        std::cin.read(buffer, sizeof(buffer));
        request.append(buffer, static_cast<std::size_t>(std::cin.gcount()));
        if (request.size() > maximumRequestBytes)
            throw std::runtime_error("retention request exceeds limit");
    }
    return request;
}

void requireExactFields(const json & value, const std::initializer_list<const char *> fields)
{
    if (!value.is_object() || value.size() != fields.size())
        throw std::runtime_error("invalid retention request");
    for (const auto field : fields)
        if (!value.contains(field))
            throw std::runtime_error("invalid retention request");
}

std::filesystem::path validatedRootDirectory(const std::string & value)
{
    if (value.empty() || value.find('\0') != std::string::npos)
        throw std::runtime_error("invalid root directory");
    const std::filesystem::path directory(value);
    if (!directory.is_absolute())
        throw std::runtime_error("invalid root directory");
    const auto canonical = std::filesystem::canonical(directory);
    if (!std::filesystem::is_directory(canonical))
        throw std::runtime_error("invalid root directory");
    const auto permissions = std::filesystem::status(canonical).permissions();
    if ((permissions & (std::filesystem::perms::group_write | std::filesystem::perms::others_write)) != std::filesystem::perms::none)
        throw std::runtime_error("invalid root directory");
    return canonical;
}

Entry parseEntry(const json & value, const std::filesystem::path & rootDirectory)
{
    requireExactFields(value, {"lease_id", "store_path"});
    if (!value.at("lease_id").is_string() || !value.at("store_path").is_string())
        throw std::runtime_error("invalid retention request");
    Entry entry {
        .leaseId = value.at("lease_id").get<std::string>(),
        .storePath = value.at("store_path").get<std::string>(),
        .rootPath = {},
    };
    if (entry.leaseId.empty() || entry.leaseId.find('\0') != std::string::npos
        || entry.leaseId.find('/') != std::string::npos || entry.leaseId.find("..") != std::string::npos)
        throw std::runtime_error("invalid retention request");
    entry.rootPath = rootDirectory / entry.leaseId;
    if (entry.rootPath.parent_path() != rootDirectory)
        throw std::runtime_error("invalid retention request");
    return entry;
}

int run(const json & request)
{
    requireExactFields(request, {"version", "store_uri", "root_directory", "entries"});
    if (!request.at("version").is_number_unsigned() || request.at("version") != 1
        || !request.at("store_uri").is_string() || !request.at("root_directory").is_string()
        || !request.at("entries").is_array())
        throw std::runtime_error("invalid retention request");
    const auto storeUri = request.at("store_uri").get<std::string>();
    if (storeUri.empty() || !storeUri.starts_with("unix://"))
        throw std::runtime_error("invalid retention request");
    const auto rootDirectory = validatedRootDirectory(request.at("root_directory").get<std::string>());
    const auto & entriesJson = request.at("entries");
    if (entriesJson.size() > maximumEntries)
        throw std::runtime_error("retention entry count exceeds limit");

    auto store = nix::openStore(storeUri);
    auto * rootStore = dynamic_cast<nix::IndirectRootStore *>(store.operator->());
    if (rootStore == nullptr)
        throw std::runtime_error("store does not support permanent roots");

    std::vector<Entry> entries;
    entries.reserve(entriesJson.size());
    for (const auto & value : entriesJson)
        entries.push_back(parseEntry(value, rootDirectory));

    json retained = json::array();
    for (const auto & entry : entries) {
        const auto storePath = store->parseStorePath(entry.storePath);
        const auto rooted = rootStore->addPermRoot(storePath, entry.rootPath);
        if (rooted != entry.rootPath)
            throw std::runtime_error("unexpected root path");
        retained.push_back({
            {"lease_id", entry.leaseId},
            {"store_path", entry.storePath},
            {"root_path", entry.rootPath.string()},
        });
    }
    const json response = {{"version", 1}, {"retained", retained}};
    const auto encoded = response.dump();
    if (encoded.size() > maximumResponseBytes)
        throw std::runtime_error("retention response exceeds limit");
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
        std::string diagnostic = "retention helper failed: ";
        diagnostic += error.what();
        diagnostic += '\n';
        std::cerr.write(diagnostic.data(), std::min(diagnostic.size(), maximumDiagnosticBytes));
        return 1;
    }
}
