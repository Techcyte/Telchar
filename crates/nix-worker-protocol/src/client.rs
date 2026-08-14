use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerTrust {
    Trusted,
    Untrusted,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerClientCapabilities {
    pub root_registration: bool,
    pub path_queries: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerPathInfo {
    deriver: Option<Vec<u8>>,
    nar_hash_hex: String,
    references: Vec<Vec<u8>>,
    registration_time: u64,
    nar_size: u64,
    ultimate: bool,
    signatures: Vec<Vec<u8>>,
    content_address: Option<Vec<u8>>,
}

impl WorkerPathInfo {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        deriver: Option<Vec<u8>>,
        nar_hash_hex: String,
        references: Vec<Vec<u8>>,
        registration_time: u64,
        nar_size: u64,
        ultimate: bool,
        signatures: Vec<Vec<u8>>,
        content_address: Option<Vec<u8>>,
    ) -> Self {
        Self {
            deriver,
            nar_hash_hex,
            references,
            registration_time,
            nar_size,
            ultimate,
            signatures,
            content_address,
        }
    }

    pub fn deriver(&self) -> Option<&[u8]> {
        self.deriver.as_deref()
    }

    pub fn nar_hash_hex(&self) -> &str {
        &self.nar_hash_hex
    }

    pub fn references(&self) -> &[Vec<u8>] {
        &self.references
    }

    pub fn registration_time(&self) -> u64 {
        self.registration_time
    }

    pub fn nar_size(&self) -> u64 {
        self.nar_size
    }

    pub fn ultimate(&self) -> bool {
        self.ultimate
    }

    pub fn signatures(&self) -> &[Vec<u8>] {
        &self.signatures
    }

    pub fn content_address(&self) -> Option<&[u8]> {
        self.content_address.as_deref()
    }
}

pub struct AddToStoreNarInfo<'a> {
    pub path: &'a [u8],
    pub deriver: Option<&'a [u8]>,
    pub nar_hash_hex: &'a str,
    pub references: &'a [Vec<u8>],
    pub registration_time: u64,
    pub nar_size: u64,
    pub ultimate: bool,
    pub signatures: &'a [Vec<u8>],
    pub content_address: Option<&'a [u8]>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorkerClientProfile {
    pub version: WorkerVersion,
    pub trust: WorkerTrust,
    pub capabilities: WorkerClientCapabilities,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildDerivationOutputRequest<'a> {
    pub name: &'a [u8],
    pub path: &'a [u8],
    pub hash_algorithm: &'a [u8],
    pub hash: &'a [u8],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuildDerivationClientRequest<'a> {
    pub drv_path: &'a [u8],
    pub outputs: &'a [BuildDerivationOutputRequest<'a>],
    pub input_sources: &'a [Vec<u8>],
    pub platform: &'a [u8],
    pub builder: &'a [u8],
    pub arguments: &'a [Vec<u8>],
    pub environment: &'a [(Vec<u8>, Vec<u8>)],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerBuildStatus {
    Built,
    AlreadyValid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerBuildResult {
    status: WorkerBuildStatus,
    outputs: Vec<(Vec<u8>, Vec<u8>)>,
}

impl WorkerBuildResult {
    pub fn status(&self) -> WorkerBuildStatus {
        self.status
    }

    pub fn outputs(&self) -> &[(Vec<u8>, Vec<u8>)] {
        &self.outputs
    }
}

pub struct WorkerClient<S> {
    stream: S,
    profile: WorkerClientProfile,
    store_directory: Vec<u8>,
}

impl<S: Read + Write> WorkerClient<S> {
    pub fn connect(stream: S) -> io::Result<Self> {
        Self::connect_with_store_directory(stream, b"/nix/store")
    }

    pub fn connect_with_store_directory(mut stream: S, store_directory: &[u8]) -> io::Result<Self> {
        validate_store_directory(store_directory)?;
        write_worker_integer_to(&mut stream, CLIENT_WORKER_MAGIC)?;
        write_worker_integer_to(&mut stream, LATEST_WORKER_VERSION.to_wire())?;
        stream.flush()?;
        if read_worker_integer_from(&mut stream)? != SERVER_WORKER_MAGIC {
            return Err(protocol_client_error());
        }
        let daemon_version = WorkerVersion::from_wire(read_worker_integer_from(&mut stream)?);
        if daemon_version.major != LATEST_WORKER_VERSION.major
            || daemon_version < MINIMUM_WORKER_VERSION
        {
            return Err(protocol_client_error());
        }
        let version = daemon_version.min(LATEST_WORKER_VERSION);
        if version >= FEATURE_NEGOTIATION_VERSION {
            write_worker_integer_to(&mut stream, 0)?;
            stream.flush()?;
            let budget = SessionAllocationBudget::new(ProtocolSessionLimits::DEFAULT);
            discard_worker_strings(&mut stream, &budget).map_err(|_| protocol_client_error())?;
        }
        if version >= WorkerVersion::new(1, 14) {
            write_worker_integer_to(&mut stream, 0)?;
        }
        if version >= WorkerVersion::new(1, 11) {
            write_worker_integer_to(&mut stream, 0)?;
        }
        stream.flush()?;
        if version >= WorkerVersion::new(1, 33) {
            discard_worker_byte_string(&mut stream, 1024)?;
        }
        let trust = if version >= WorkerVersion::new(1, 35) {
            match read_worker_integer_from(&mut stream)? {
                0 => WorkerTrust::Unknown,
                1 => WorkerTrust::Trusted,
                2 => WorkerTrust::Untrusted,
                _ => return Err(protocol_client_error()),
            }
        } else {
            WorkerTrust::Unknown
        };
        read_operation_frames(&mut stream, version)?;
        Ok(Self {
            stream,
            profile: WorkerClientProfile {
                version,
                trust,
                capabilities: WorkerClientCapabilities {
                    root_registration: true,
                    path_queries: true,
                },
            },
            store_directory: store_directory.to_vec(),
        })
    }

    pub fn profile(&self) -> &WorkerClientProfile {
        &self.profile
    }

    pub fn is_valid_path(&mut self, path: &[u8]) -> io::Result<bool> {
        self.write_store_path_operation(WorkerOperation::IsValidPath, path)?;
        read_operation_frames(&mut self.stream, self.profile.version)?;
        read_strict_client_boolean(&mut self.stream)
    }

    pub fn query_path_info(&mut self, path: &[u8]) -> io::Result<Option<WorkerPathInfo>> {
        self.write_store_path_operation(WorkerOperation::QueryPathInfo, path)?;
        read_operation_frames(&mut self.stream, self.profile.version)?;
        if !read_strict_client_boolean(&mut self.stream)? {
            return Ok(None);
        }
        read_worker_path_info(&mut self.stream)
            .map(Some)
            .map_err(|_| protocol_client_error())
    }

    pub fn build_derivation(
        &mut self,
        request: &BuildDerivationClientRequest<'_>,
        logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
    ) -> io::Result<WorkerBuildResult> {
        validate_build_derivation_request(request, self.profile.trust)?;
        write_worker_integer_to(&mut self.stream, WorkerOperation::BuildDerivation.code())?;
        write_worker_byte_string_to(&mut self.stream, request.drv_path)?;
        write_worker_integer_to(&mut self.stream, request.outputs.len() as u64)?;
        for output in request.outputs {
            write_worker_byte_string_to(&mut self.stream, output.name)?;
            write_worker_byte_string_to(&mut self.stream, output.path)?;
            write_worker_byte_string_to(&mut self.stream, output.hash_algorithm)?;
            write_worker_byte_string_to(&mut self.stream, output.hash)?;
        }
        write_byte_string_collection(&mut self.stream, request.input_sources)?;
        write_worker_byte_string_to(&mut self.stream, request.platform)?;
        write_worker_byte_string_to(&mut self.stream, request.builder)?;
        write_byte_string_collection(&mut self.stream, request.arguments)?;
        write_worker_integer_to(&mut self.stream, request.environment.len() as u64)?;
        for (key, value) in request.environment {
            write_worker_byte_string_to(&mut self.stream, key)?;
            write_worker_byte_string_to(&mut self.stream, value)?;
        }
        write_worker_integer_to(&mut self.stream, 0)?;
        self.stream.flush()?;
        read_build_operation_frames(&mut self.stream, self.profile.version, logs)?;
        read_worker_build_result(&mut self.stream, self.profile.version)
            .map_err(|_| protocol_client_error())
    }

    pub fn nar_from_path(
        &mut self,
        path: &[u8],
        nar_size: u64,
        sink: &mut dyn Write,
    ) -> io::Result<()> {
        self.write_store_path_operation(WorkerOperation::NarFromPath, path)?;
        read_operation_frames(&mut self.stream, self.profile.version)?;
        let copied = io::copy(&mut Read::by_ref(&mut self.stream).take(nar_size), sink)
            .map_err(|_| protocol_client_error())?;
        if copied != nar_size {
            return Err(protocol_client_error());
        }
        Ok(())
    }

    pub fn add_to_store_nar(
        &mut self,
        info: &AddToStoreNarInfo<'_>,
        source: &mut dyn Read,
        repair: bool,
        dont_check_signatures: bool,
    ) -> io::Result<()> {
        validate_add_to_store_nar(info, repair, dont_check_signatures, self.profile.trust)?;
        write_worker_integer_to(&mut self.stream, WorkerOperation::AddToStoreNar.code())?;
        write_worker_byte_string_to(&mut self.stream, info.path)?;
        write_worker_byte_string_to(&mut self.stream, info.deriver.unwrap_or_default())?;
        write_worker_byte_string_to(&mut self.stream, info.nar_hash_hex.as_bytes())?;
        write_worker_integer_to(&mut self.stream, info.references.len() as u64)?;
        for reference in info.references {
            write_worker_byte_string_to(&mut self.stream, reference)?;
        }
        write_worker_integer_to(&mut self.stream, info.registration_time)?;
        write_worker_integer_to(&mut self.stream, info.nar_size)?;
        write_worker_integer_to(&mut self.stream, u64::from(info.ultimate))?;
        write_worker_integer_to(&mut self.stream, info.signatures.len() as u64)?;
        for signature in info.signatures {
            write_worker_byte_string_to(&mut self.stream, signature)?;
        }
        write_worker_byte_string_to(&mut self.stream, info.content_address.unwrap_or_default())?;
        write_worker_integer_to(&mut self.stream, u64::from(repair))?;
        write_worker_integer_to(&mut self.stream, u64::from(dont_check_signatures))?;
        self.stream.flush()?;
        let mut buffer = [0_u8; 8192];
        loop {
            let read = source
                .read(&mut buffer)
                .map_err(|_| protocol_client_error())?;
            if read == 0 {
                break;
            }
            write_worker_integer_to(&mut self.stream, read as u64)?;
            self.stream.write_all(&buffer[..read])?;
        }
        write_worker_integer_to(&mut self.stream, 0)?;
        self.stream.flush()?;
        read_operation_frames(&mut self.stream, self.profile.version)
    }

    pub fn ensure_path(&mut self, store_path: &[u8]) -> io::Result<()> {
        validate_store_path_in_directory(store_path, &self.store_directory)
            .map_err(|_| protocol_client_error())?;
        self.execute_path_operation(WorkerOperation::EnsurePath, store_path)
    }

    pub fn add_temporary_root(&mut self, store_path: &[u8]) -> io::Result<()> {
        validate_store_path_in_directory(store_path, &self.store_directory)
            .map_err(|_| protocol_client_error())?;
        self.execute_path_operation(WorkerOperation::AddTempRoot, store_path)
    }

    pub fn add_indirect_root(&mut self, root_path: &[u8]) -> io::Result<()> {
        if root_path.is_empty()
            || root_path.len() > MAXIMUM_WORKER_STORE_PATH_BYTES * 4
            || !root_path.starts_with(b"/")
            || root_path.contains(&0)
        {
            return Err(protocol_client_error());
        }
        self.execute_path_operation(WorkerOperation::AddIndirectRoot, root_path)
    }

    pub fn into_inner(self) -> S {
        self.stream
    }

    fn write_store_path_operation(
        &mut self,
        operation: WorkerOperation,
        path: &[u8],
    ) -> io::Result<()> {
        validate_store_path(path).map_err(|_| protocol_client_error())?;
        write_worker_integer_to(&mut self.stream, operation.code())?;
        write_worker_byte_string_to(&mut self.stream, path)?;
        self.stream.flush()?;
        Ok(())
    }

    fn execute_path_operation(
        &mut self,
        operation: WorkerOperation,
        path: &[u8],
    ) -> io::Result<()> {
        write_worker_integer_to(&mut self.stream, operation.code())?;
        write_worker_byte_string_to(&mut self.stream, path)?;
        self.stream.flush()?;
        read_operation_frames(&mut self.stream, self.profile.version)?;
        if read_worker_integer_from(&mut self.stream)? != 1 {
            return Err(protocol_client_error());
        }
        Ok(())
    }
}

fn validate_build_derivation_request(
    request: &BuildDerivationClientRequest<'_>,
    trust: WorkerTrust,
) -> io::Result<()> {
    if trust != WorkerTrust::Trusted
        || request.outputs.is_empty()
        || request.outputs.len() > MAXIMUM_BUILD_DERIVATION_OUTPUTS
        || request.input_sources.len() > MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES
        || request.arguments.len() > MAXIMUM_BUILD_DERIVATION_ARGUMENTS
        || request.environment.len() > MAXIMUM_BUILD_DERIVATION_ENVIRONMENT
    {
        return Err(protocol_client_error());
    }
    validate_store_path(request.drv_path)?;
    if !request.drv_path.ends_with(b".drv")
        || request.platform.is_empty()
        || request.platform.len() > MAXIMUM_BUILD_DERIVATION_PLATFORM_BYTES
        || request.builder.is_empty()
        || request.builder.len() > MAXIMUM_BUILD_DERIVATION_BUILDER_BYTES
    {
        return Err(protocol_client_error());
    }
    let mut output_names = Vec::with_capacity(request.outputs.len());
    for output in request.outputs {
        if output.name.is_empty()
            || output.name.len() > MAXIMUM_BUILD_DERIVATION_OUTPUT_NAME_BYTES
            || output_names.contains(&output.name)
        {
            return Err(protocol_client_error());
        }
        validate_store_path(output.path)?;
        validate_build_output_hash(output.hash_algorithm, output.hash)?;
        output_names.push(output.name);
    }
    for path in request.input_sources {
        validate_store_path(path)?;
    }
    for argument in request.arguments {
        if argument.len() > MAXIMUM_BUILD_DERIVATION_ARGUMENT_BYTES {
            return Err(protocol_client_error());
        }
    }
    let mut environment_keys = Vec::with_capacity(request.environment.len());
    for (key, value) in request.environment {
        if key.is_empty()
            || key.len() > MAXIMUM_BUILD_DERIVATION_ENVIRONMENT_KEY_BYTES
            || value.len() > MAXIMUM_BUILD_DERIVATION_ENVIRONMENT_VALUE_BYTES
            || environment_keys.contains(&key.as_slice())
        {
            return Err(protocol_client_error());
        }
        environment_keys.push(key.as_slice());
    }
    Ok(())
}

fn write_byte_string_collection(output: &mut impl Write, values: &[Vec<u8>]) -> io::Result<()> {
    write_worker_integer_to(output, values.len() as u64)?;
    for value in values {
        write_worker_byte_string_to(output, value)?;
    }
    Ok(())
}

fn read_build_operation_frames(
    input: &mut impl Read,
    version: WorkerVersion,
    logs: &mut dyn FnMut(&[u8]) -> io::Result<()>,
) -> io::Result<()> {
    loop {
        match read_worker_integer_from(input)? {
            STDERR_NEXT => {
                let message =
                    read_worker_byte_string_from(input, MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES)?;
                logs(&message).map_err(|_| protocol_client_error())?;
            }
            STDERR_START_ACTIVITY => {
                read_worker_integer_from(input)?;
                read_worker_integer_from(input)?;
                read_worker_integer_from(input)?;
                discard_worker_byte_string(input, MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES)?;
                discard_activity_fields(input)?;
                read_worker_integer_from(input)?;
            }
            STDERR_STOP_ACTIVITY => {
                read_worker_integer_from(input)?;
            }
            STDERR_RESULT => {
                read_worker_integer_from(input)?;
                read_worker_integer_from(input)?;
                discard_activity_fields(input)?;
            }
            STDERR_ERROR => {
                if version >= WorkerVersion::new(1, 26) {
                    discard_worker_error(input, version)?;
                } else {
                    discard_worker_byte_string(input, MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES)?;
                    read_worker_integer_from(input)?;
                }
                return Err(protocol_client_error());
            }
            STDERR_LAST => return Ok(()),
            _ => return Err(protocol_client_error()),
        }
    }
}

fn read_worker_build_result(
    input: &mut impl Read,
    version: WorkerVersion,
) -> io::Result<WorkerBuildResult> {
    let raw_status = read_worker_integer_from(input)?;
    let message = read_worker_byte_string_from(input, MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES)?;
    let status = match raw_status {
        0 => WorkerBuildStatus::Built,
        2 => WorkerBuildStatus::AlreadyValid,
        1 | 3..=14 => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "unsupported build result status {raw_status}: {}",
                    String::from_utf8_lossy(&message)
                ),
            ));
        }
        _ => return Err(protocol_client_error()),
    };
    if version >= WorkerVersion::new(1, 29) {
        read_worker_integer_from(input)
            .map_err(|error| io::Error::new(error.kind(), format!("times built: {error}")))?;
        read_strict_client_boolean(input)
            .map_err(|error| io::Error::new(error.kind(), format!("determinism: {error}")))?;
        read_worker_integer_from(input)
            .map_err(|error| io::Error::new(error.kind(), format!("start time: {error}")))?;
        read_worker_integer_from(input)
            .map_err(|error| io::Error::new(error.kind(), format!("stop time: {error}")))?;
    }
    if version >= WorkerVersion::new(1, 37) {
        read_optional_duration(input)
            .map_err(|error| io::Error::new(error.kind(), format!("user duration: {error}")))?;
        read_optional_duration(input)
            .map_err(|error| io::Error::new(error.kind(), format!("system duration: {error}")))?;
    }
    let outputs = if version >= WorkerVersion::new(1, 28) {
        read_built_outputs(input)
            .map_err(|error| io::Error::new(error.kind(), format!("built outputs: {error}")))?
    } else {
        Vec::new()
    };
    Ok(WorkerBuildResult { status, outputs })
}

fn read_optional_duration(input: &mut impl Read) -> io::Result<()> {
    match read_worker_integer_from(input)? {
        0 => Ok(()),
        1 => {
            read_worker_integer_from(input)?;
            Ok(())
        }
        _ => Err(protocol_client_error()),
    }
}

fn read_built_outputs(input: &mut impl Read) -> io::Result<Vec<(Vec<u8>, Vec<u8>)>> {
    let count =
        usize::try_from(read_worker_integer_from(input)?).map_err(|_| protocol_client_error())?;
    if count > MAXIMUM_BUILD_DERIVATION_OUTPUTS {
        return Err(protocol_client_error());
    }
    let mut outputs: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(count);
    for _ in 0..count {
        let output_id = read_worker_byte_string_from(input, 256)?;
        let realisation =
            read_worker_byte_string_from(input, MAXIMUM_BUILD_DERIVATION_ENVIRONMENT_VALUE_BYTES)?;
        let output_name = output_id
            .rsplit(|byte| *byte == b'!')
            .next()
            .filter(|name| !name.is_empty())
            .ok_or_else(protocol_client_error)?
            .to_vec();
        let value: serde_json::Value =
            serde_json::from_slice(&realisation).map_err(|_| protocol_client_error())?;
        let value = value.get("value").unwrap_or(&value);
        let path: Vec<u8> = value
            .get("outPath")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(protocol_client_error)?
            .bytes()
            .collect();
        let path = if path.starts_with(b"/nix/store/") {
            path
        } else {
            let mut absolute = b"/nix/store/".to_vec();
            absolute.extend_from_slice(&path);
            absolute
        };
        validate_store_path(&path)?;
        if outputs.iter().any(|(name, _)| name == &output_name) {
            return Err(protocol_client_error());
        }
        outputs.push((output_name, path));
    }
    outputs.sort();
    Ok(outputs)
}

fn validate_add_to_store_nar(
    info: &AddToStoreNarInfo<'_>,
    repair: bool,
    dont_check_signatures: bool,
    trust: WorkerTrust,
) -> io::Result<()> {
    validate_store_path(info.path).map_err(|_| protocol_client_error())?;
    if let Some(deriver) = info.deriver {
        validate_store_path(deriver).map_err(|_| protocol_client_error())?;
    }
    if info.nar_hash_hex.len() != MAXIMUM_ADD_MULTIPLE_TO_STORE_HASH_BYTES
        || !info
            .nar_hash_hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || info
            .nar_hash_hex
            .bytes()
            .any(|byte| byte.is_ascii_uppercase())
        || info.references.len() > MAXIMUM_ADD_MULTIPLE_TO_STORE_REFERENCES
        || info.signatures.len() > MAXIMUM_ADD_MULTIPLE_TO_STORE_SIGNATURES
        || info.ultimate
        || repair
        || (dont_check_signatures && trust != WorkerTrust::Trusted)
    {
        return Err(protocol_client_error());
    }
    let mut references = std::collections::BTreeSet::new();
    for reference in info.references {
        validate_store_path(reference).map_err(|_| protocol_client_error())?;
        if !references.insert(reference) {
            return Err(protocol_client_error());
        }
    }
    let mut signatures = std::collections::BTreeSet::new();
    for signature in info.signatures {
        if signature.len() > MAXIMUM_ADD_MULTIPLE_TO_STORE_SIGNATURE_BYTES
            || !signatures.insert(signature)
        {
            return Err(protocol_client_error());
        }
    }
    if info
        .content_address
        .is_some_and(|value| value.len() > MAXIMUM_ADD_MULTIPLE_TO_STORE_CONTENT_ADDRESS_BYTES)
    {
        return Err(protocol_client_error());
    }
    Ok(())
}

fn read_strict_client_boolean(input: &mut impl Read) -> io::Result<bool> {
    match read_worker_integer_from(input)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(protocol_client_error()),
    }
}

fn read_worker_path_info(input: &mut impl Read) -> io::Result<WorkerPathInfo> {
    let budget = SessionAllocationBudget::new(ProtocolSessionLimits::DEFAULT);
    let (deriver, _deriver_charge) =
        read_worker_byte_string_with_charge_from(input, MAXIMUM_WORKER_STORE_PATH_BYTES, &budget)?;
    let deriver = if deriver.is_empty() {
        None
    } else {
        validate_store_path(&deriver)?;
        Some(deriver)
    };
    let (nar_hash, _hash_charge) = read_worker_byte_string_with_charge_from(
        input,
        MAXIMUM_ADD_MULTIPLE_TO_STORE_HASH_BYTES,
        &budget,
    )?;
    if nar_hash.len() != MAXIMUM_ADD_MULTIPLE_TO_STORE_HASH_BYTES
        || !nar_hash.iter().all(u8::is_ascii_hexdigit)
        || nar_hash.iter().any(u8::is_ascii_uppercase)
    {
        return Err(protocol_client_error());
    }
    let nar_hash_hex = String::from_utf8(nar_hash).map_err(|_| protocol_client_error())?;
    let references = read_client_byte_string_set(
        input,
        MAXIMUM_ADD_MULTIPLE_TO_STORE_REFERENCES,
        MAXIMUM_WORKER_STORE_PATH_BYTES,
        &budget,
        true,
    )?;
    let registration_time = read_worker_integer_from(input)?;
    let nar_size = read_worker_integer_from(input)?;
    let ultimate = read_strict_client_boolean(input)?;
    let signatures = read_client_byte_string_set(
        input,
        MAXIMUM_ADD_MULTIPLE_TO_STORE_SIGNATURES,
        MAXIMUM_ADD_MULTIPLE_TO_STORE_SIGNATURE_BYTES,
        &budget,
        false,
    )?;
    let (content_address, _content_address_charge) = read_worker_byte_string_with_charge_from(
        input,
        MAXIMUM_ADD_MULTIPLE_TO_STORE_CONTENT_ADDRESS_BYTES,
        &budget,
    )?;
    Ok(WorkerPathInfo::new(
        deriver,
        nar_hash_hex,
        references,
        registration_time,
        nar_size,
        ultimate,
        signatures,
        (!content_address.is_empty()).then_some(content_address),
    ))
}

fn read_client_byte_string_set(
    input: &mut impl Read,
    maximum_count: usize,
    maximum_length: usize,
    budget: &SessionAllocationBudget,
    validate_paths: bool,
) -> io::Result<Vec<Vec<u8>>> {
    let count =
        usize::try_from(read_worker_integer_from(input)?).map_err(|_| protocol_client_error())?;
    if count > maximum_count {
        return Err(protocol_client_error());
    }
    let _collection_charge = budget
        .charge(
            count
                .checked_mul(std::mem::size_of::<Vec<u8>>())
                .ok_or_else(protocol_client_error)?,
        )
        .map_err(|_| protocol_client_error())?;
    let mut values = Vec::with_capacity(count);
    let mut charges = Vec::with_capacity(count);
    for _ in 0..count {
        let (value, charge) =
            read_worker_byte_string_with_charge_from(input, maximum_length, budget)?;
        if validate_paths {
            validate_store_path(&value)?;
        }
        if values.contains(&value) {
            return Err(protocol_client_error());
        }
        values.push(value);
        charges.push(charge);
    }
    drop(charges);
    Ok(values)
}

fn read_operation_frames(input: &mut impl Read, version: WorkerVersion) -> io::Result<()> {
    loop {
        match read_worker_integer_from(input)? {
            STDERR_NEXT => {
                discard_worker_byte_string(input, MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES)?
            }
            STDERR_START_ACTIVITY => {
                read_worker_integer_from(input)?;
                read_worker_integer_from(input)?;
                read_worker_integer_from(input)?;
                discard_worker_byte_string(input, MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES)?;
                discard_activity_fields(input)?;
                read_worker_integer_from(input)?;
            }
            STDERR_STOP_ACTIVITY => {
                read_worker_integer_from(input)?;
            }
            STDERR_RESULT => {
                read_worker_integer_from(input)?;
                read_worker_integer_from(input)?;
                discard_activity_fields(input)?;
            }
            STDERR_ERROR => {
                if version >= WorkerVersion::new(1, 26) {
                    discard_worker_error(input, version)?;
                } else {
                    discard_worker_byte_string(input, MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES)?;
                    read_worker_integer_from(input)?;
                }
                return Err(protocol_client_error());
            }
            STDERR_LAST => return Ok(()),
            _ => return Err(protocol_client_error()),
        }
    }
}

fn discard_activity_fields(input: &mut impl Read) -> io::Result<()> {
    let count =
        usize::try_from(read_worker_integer_from(input)?).map_err(|_| protocol_client_error())?;
    if count > MAXIMUM_STRUCTURED_FRAME_FIELDS {
        return Err(protocol_client_error());
    }
    for _ in 0..count {
        match read_worker_integer_from(input)? {
            0 => {
                read_worker_integer_from(input)?;
            }
            1 => discard_worker_byte_string(input, MAXIMUM_STRUCTURED_FRAME_FIELD_BYTES)?,
            _ => return Err(protocol_client_error()),
        }
    }
    Ok(())
}

fn discard_worker_error(input: &mut impl Read, version: WorkerVersion) -> io::Result<()> {
    discard_worker_byte_string(input, 256)?;
    read_worker_integer_from(input)?;
    discard_worker_byte_string(input, 256)?;
    discard_worker_byte_string(input, MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES)?;
    read_worker_integer_from(input)?;
    if version >= WorkerVersion::new(1, 26) {
        let trace_count = usize::try_from(read_worker_integer_from(input)?)
            .map_err(|_| protocol_client_error())?;
        if trace_count > MAXIMUM_STRUCTURED_FRAME_FIELDS {
            return Err(protocol_client_error());
        }
        for _ in 0..trace_count {
            read_worker_integer_from(input)?;
            discard_worker_byte_string(input, MAXIMUM_STRUCTURED_FRAME_MESSAGE_BYTES)?;
        }
    }
    Ok(())
}

fn discard_worker_byte_string(input: &mut impl Read, maximum: usize) -> io::Result<()> {
    read_worker_byte_string_from(input, maximum).map(|_| ())
}

fn read_worker_byte_string_from(input: &mut impl Read, maximum: usize) -> io::Result<Vec<u8>> {
    let length =
        usize::try_from(read_worker_integer_from(input)?).map_err(|_| protocol_client_error())?;
    if length > maximum {
        return Err(protocol_client_error());
    }
    let padding_length = (8 - length % 8) % 8;
    let framed_length = length
        .checked_add(padding_length)
        .ok_or_else(protocol_client_error)?;
    let mut framed = vec![0_u8; framed_length];
    input.read_exact(&mut framed)?;
    if framed[length..].iter().any(|byte| *byte != 0) {
        return Err(protocol_client_error());
    }
    framed.truncate(length);
    Ok(framed)
}

fn protocol_client_error() -> io::Error {
    io::Error::other("Nix daemon operation failed")
}

pub(super) fn validate_store_path(path: &[u8]) -> io::Result<()> {
    validate_store_path_in_directory(path, NIX_STORE_DIRECTORY.strip_suffix(b"/").unwrap())
}

fn validate_store_directory(directory: &[u8]) -> io::Result<()> {
    if directory.is_empty()
        || directory.len() >= MAXIMUM_WORKER_STORE_PATH_BYTES
        || !directory.starts_with(b"/")
        || directory.ends_with(b"/")
        || directory.contains(&0)
    {
        return Err(protocol_client_error());
    }
    Ok(())
}

fn validate_store_path_in_directory(path: &[u8], directory: &[u8]) -> io::Result<()> {
    validate_store_directory(directory)?;
    let prefix_length = directory.len() + 1;
    if path.len() > MAXIMUM_WORKER_STORE_PATH_BYTES
        || !path.starts_with(directory)
        || path.get(directory.len()) != Some(&b'/')
        || path[prefix_length..].contains(&b'/')
        || path.len() <= prefix_length + NIX_STORE_HASH_LENGTH + 1
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid QueryValidPaths request",
        ));
    }
    let base = path.rsplit(|byte| *byte == b'/').next().unwrap_or_default();
    if base.len() <= NIX_STORE_HASH_LENGTH + 1
        || base[NIX_STORE_HASH_LENGTH] != b'-'
        || !base[..NIX_STORE_HASH_LENGTH]
            .iter()
            .all(|byte| NIX_STORE_HASH_ALPHABET.contains(byte))
        || !base[NIX_STORE_HASH_LENGTH + 1..].iter().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.' | b'_' | b'?' | b'=')
        })
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid QueryValidPaths request",
        ));
    }
    Ok(())
}
