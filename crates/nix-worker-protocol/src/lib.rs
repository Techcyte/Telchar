//! Implements bounded typed Nix worker-protocol framing, negotiation, operations, daemon calls, and build results.

#![forbid(unsafe_code)]

use std::io::{self, Read, Write};

mod protocol;
pub use protocol::*;

mod stderr;
pub use stderr::*;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixtureSetOptions {
    pub override_count: u64,
    pub string_lengths: Vec<u64>,
}

mod fixture;

pub use fixture::*;

pub trait WorkerInput: Read {
    fn complete_message(&mut self) {}

    fn has_unread_message_data(&self) -> bool {
        false
    }
}

impl WorkerInput for &[u8] {
    fn has_unread_message_data(&self) -> bool {
        !self.is_empty()
    }
}

impl<R: WorkerInput + ?Sized> WorkerInput for &mut R {
    fn complete_message(&mut self) {
        (**self).complete_message();
    }

    fn has_unread_message_data(&self) -> bool {
        (**self).has_unread_message_data()
    }
}

mod requests;
pub use requests::*;

pub struct WorkerReader<R> {
    input: R,
    budget: SessionAllocationBudget,
}

impl<R: WorkerInput> WorkerReader<R> {
    pub fn new(input: R, limits: ProtocolSessionLimits) -> Self {
        Self {
            input,
            budget: SessionAllocationBudget::new(limits),
        }
    }

    pub fn retained_metadata_bytes(&self) -> usize {
        self.budget.retained_bytes()
    }

    pub fn perform_server_handshake<W: Write>(
        &mut self,
        output: &mut W,
        server_features: &[String],
    ) -> io::Result<NegotiatedWorkerVersion> {
        if self.read_integer()? != CLIENT_WORKER_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "worker handshake magic mismatch",
            ));
        }

        write_worker_integer_to(output, SERVER_WORKER_MAGIC)?;
        write_worker_integer_to(output, LATEST_WORKER_VERSION.to_wire())?;
        output.flush()?;

        let client_version = WorkerVersion::from_wire(self.read_integer()?);
        let client_features =
            if client_version.min(LATEST_WORKER_VERSION) >= FEATURE_NEGOTIATION_VERSION {
                Some(self.read_strings()?)
            } else {
                None
            };
        let negotiated = negotiate_worker_version_with_budget(
            client_version,
            client_features
                .as_ref()
                .map(|features| features.values.as_slice())
                .unwrap_or_default(),
            server_features,
            &self.budget,
        )
        .map_err(|error| match error {
            ProtocolError::SizeLimit => io::Error::new(
                io::ErrorKind::InvalidData,
                "worker metadata exceeds session limit",
            ),
            _ => io::Error::new(io::ErrorKind::InvalidData, "unsupported worker version"),
        })?;
        drop(client_features);

        if negotiated.version >= FEATURE_NEGOTIATION_VERSION {
            write_worker_strings_to(output, server_features)?;
            output.flush()?;
        }

        self.input.complete_message();
        Ok(negotiated)
    }

    pub fn complete_server_post_handshake<W: Write>(
        &mut self,
        output: &mut W,
        version: WorkerVersion,
        daemon_version: &str,
    ) -> io::Result<()> {
        if version >= WorkerVersion::new(1, 14) && self.read_integer()? != 0 {
            self.read_integer()?;
        }
        if version >= WorkerVersion::new(1, 11) {
            self.read_integer()?;
        }
        if version >= WorkerVersion::new(1, 33) {
            write_worker_byte_string_to(output, daemon_version.as_bytes())?;
        }
        if version >= WorkerVersion::new(1, 35) {
            write_worker_integer_to(output, 0)?;
        }
        write_worker_integer_to(output, STDERR_LAST)?;
        output.flush()?;
        self.input.complete_message();
        Ok(())
    }

    pub fn read_operation(&mut self) -> io::Result<WorkerOperation> {
        worker_operation_from_code(self.read_integer()?)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "unknown worker operation"))
    }

    pub fn complete_store_path_request(&mut self) -> io::Result<StorePathRequest> {
        let (path, charge) = read_worker_byte_string_with_charge_from(
            &mut self.input,
            MAXIMUM_WORKER_STORE_PATH_BYTES,
            &self.budget,
        )?;
        validate_store_path(&path)?;
        self.input.complete_message();
        Ok(StorePathRequest {
            path,
            _charge: charge,
        })
    }

    pub fn complete_query_valid_paths(
        &mut self,
        version: WorkerVersion,
    ) -> io::Result<QueryValidPathsRequest> {
        let count = usize::try_from(self.read_integer()?).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid QueryValidPaths request",
            )
        })?;
        if count > MAXIMUM_QUERY_VALID_PATHS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid QueryValidPaths request",
            ));
        }
        let collection_charge = self
            .budget
            .charge(
                count
                    .checked_mul(std::mem::size_of::<Vec<u8>>())
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            "invalid QueryValidPaths request",
                        )
                    })?,
            )
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid QueryValidPaths request",
                )
            })?;
        let mut paths = Vec::with_capacity(count);
        let mut value_charges = Vec::with_capacity(count);
        for _ in 0..count {
            let (path, charge) = read_worker_byte_string_with_charge_from(
                &mut self.input,
                MAXIMUM_WORKER_STORE_PATH_BYTES,
                &self.budget,
            )
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid QueryValidPaths request",
                )
            })?;
            validate_store_path(&path)?;
            if paths.iter().any(|existing| existing == &path) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid QueryValidPaths request",
                ));
            }
            paths.push(path);
            value_charges.push(charge);
        }
        let substitute = if version >= WorkerVersion::new(1, 27) {
            match self.read_integer()? {
                0 => false,
                1 => true,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid QueryValidPaths request",
                    ));
                }
            }
        } else {
            false
        };
        self.input.complete_message();
        Ok(QueryValidPathsRequest {
            paths,
            substitute,
            _collection_charge: collection_charge,
            _value_charges: value_charges,
        })
    }

    pub fn complete_empty_add_multiple_to_store(
        &mut self,
        version: WorkerVersion,
    ) -> Result<EmptyAddMultipleToStoreRequest, AddMultipleToStoreRequestError> {
        self.complete_add_multiple_to_store(version, |_, _| {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "nonempty AddMultipleToStore is unsupported",
            ))
        })
    }

    pub fn complete_add_multiple_to_store<F>(
        &mut self,
        version: WorkerVersion,
        mut receive: F,
    ) -> Result<AddMultipleToStoreRequest, AddMultipleToStoreRequestError>
    where
        F: FnMut(&AddMultipleToStorePathInfo, &mut dyn Read) -> io::Result<()>,
    {
        if version < WorkerVersion::new(1, 32) {
            return Err(AddMultipleToStoreRequestError(
                io::ErrorKind::InvalidInput,
                "AddMultipleToStore requires worker protocol 1.32".to_owned(),
            ));
        }
        let repair = read_strict_worker_boolean(&mut self.input, "repair")?;
        let dont_check_signatures = read_strict_worker_boolean(&mut self.input, "dontCheckSigs")?;
        if repair {
            return Err(AddMultipleToStoreRequestError(
                io::ErrorKind::InvalidInput,
                "repair is unsupported for AddMultipleToStore".to_owned(),
            ));
        }

        let budget = self.budget.clone();
        let mut source = FramedReader::new(&mut self.input);
        let object_count = read_bounded_count(&mut source, MAXIMUM_ADD_MULTIPLE_TO_STORE_OBJECTS)?;
        for _ in 0..object_count {
            let info = read_add_multiple_path_info(&mut source, &budget)?;
            let mut nar = (&mut source).take(info.nar_size);
            receive(&info, &mut nar)?;
            if nar.limit() != 0 {
                return Err(AddMultipleToStoreRequestError(
                    io::ErrorKind::UnexpectedEof,
                    "AddMultipleToStore NAR body is truncated".to_owned(),
                ));
            }
        }
        let mut trailing = [0_u8; 1];
        if source.read(&mut trailing)? != 0 {
            return Err(AddMultipleToStoreRequestError(
                io::ErrorKind::InvalidData,
                "trailing AddMultipleToStore logical bytes".to_owned(),
            ));
        }
        self.input.complete_message();
        Ok(AddMultipleToStoreRequest {
            repair,
            dont_check_signatures,
            object_count,
        })
    }

    pub fn complete_query_missing(&mut self) -> io::Result<QueryMissingRequest> {
        let request = self.read_derived_paths()?;
        if self.input.has_unread_message_data() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trailing QueryMissing request data",
            ));
        }
        self.input.complete_message();
        Ok(request)
    }

    fn read_derived_paths(&mut self) -> io::Result<QueryMissingRequest> {
        let count_value = self.read_integer()?;
        let count = usize::try_from(count_value).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("invalid QueryMissing count: {count_value}"),
            )
        })?;
        if count > MAXIMUM_QUERY_VALID_PATHS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("QueryMissing count exceeds limit: {count}"),
            ));
        }
        let collection_charge = self
            .budget
            .charge(
                count
                    .checked_mul(std::mem::size_of::<Vec<u8>>())
                    .ok_or_else(invalid_query_missing_request)?,
            )
            .map_err(|_| invalid_query_missing_request())?;
        let mut targets = Vec::with_capacity(count);
        let mut value_charges = Vec::with_capacity(count);
        for _ in 0..count {
            let (target, charge) = read_worker_byte_string_with_charge_from(
                &mut self.input,
                MAXIMUM_WORKER_STORE_PATH_BYTES + 1 + MAXIMUM_BUILD_DERIVATION_OUTPUT_NAME_BYTES,
                &self.budget,
            )?;
            validate_derived_path(&target)?;
            if targets.iter().any(|existing| existing == &target) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "duplicate QueryMissing target",
                ));
            }
            targets.push(target);
            value_charges.push(charge);
        }
        Ok(QueryMissingRequest {
            targets,
            _collection_charge: collection_charge,
            _value_charges: value_charges,
        })
    }

    pub fn complete_build_paths_with_results(
        &mut self,
    ) -> io::Result<BuildPathsWithResultsRequest> {
        let missing = self.read_derived_paths()?;
        let build_mode = self.read_integer()?;
        if build_mode > 2 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid BuildPathsWithResults request",
            ));
        }
        if self.input.has_unread_message_data() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trailing BuildPathsWithResults request data",
            ));
        }
        self.input.complete_message();
        Ok(BuildPathsWithResultsRequest {
            targets: missing.targets,
            build_mode,
            _collection_charge: missing._collection_charge,
            _value_charges: missing._value_charges,
        })
    }

    pub fn complete_build_derivation(&mut self) -> io::Result<BuildDerivationRequest> {
        let invalid = |message: &'static str| io::Error::new(io::ErrorKind::InvalidData, message);
        let (drv_path, drv_charge) = read_build_string(
            &mut self.input,
            MAXIMUM_WORKER_STORE_PATH_BYTES,
            &self.budget,
        )
        .map_err(|_| invalid("invalid BuildDerivation request"))?;
        validate_build_store_path(&drv_path)
            .map_err(|_| invalid("invalid BuildDerivation request"))?;
        if !drv_path.ends_with(b".drv") {
            return Err(invalid("invalid BuildDerivation request"));
        }

        let (outputs, output_collection_charge) = read_build_count(
            &mut self.input,
            MAXIMUM_BUILD_DERIVATION_OUTPUTS,
            std::mem::size_of::<BuildDerivationOutput>(),
            &self.budget,
        )?;
        let mut output_values: Vec<BuildDerivationOutput> = Vec::with_capacity(outputs);
        let mut string_charges = Vec::new();
        for _ in 0..outputs {
            let (name, name_charge) = read_build_string(
                &mut self.input,
                MAXIMUM_BUILD_DERIVATION_OUTPUT_NAME_BYTES,
                &self.budget,
            )?;
            if name.is_empty()
                || name.contains(&0)
                || output_values.iter().any(|output| output.name == name)
            {
                return Err(invalid("invalid BuildDerivation request"));
            }
            let (path, path_charge) = read_build_string(
                &mut self.input,
                MAXIMUM_WORKER_STORE_PATH_BYTES,
                &self.budget,
            )?;
            validate_build_store_path(&path)
                .map_err(|_| invalid("invalid BuildDerivation request"))?;
            if output_values.iter().any(|output| output.path == path) {
                return Err(invalid("invalid BuildDerivation request"));
            }
            let (hash_algorithm, hash_algorithm_charge) = read_build_string(
                &mut self.input,
                MAXIMUM_BUILD_DERIVATION_HASH_ALGORITHM_BYTES,
                &self.budget,
            )?;
            let (hash, hash_charge) = read_build_string(
                &mut self.input,
                MAXIMUM_BUILD_DERIVATION_HASH_BYTES,
                &self.budget,
            )?;
            validate_build_output_hash(&hash_algorithm, &hash)
                .map_err(|_| invalid("invalid BuildDerivation request"))?;
            output_values.push(BuildDerivationOutput {
                name,
                path,
                hash_algorithm,
                hash,
                _charges: BuildDerivationStringCharges {
                    _charges: vec![name_charge, path_charge, hash_algorithm_charge, hash_charge],
                },
            });
        }

        let (input_count, input_collection_charge) = read_build_count(
            &mut self.input,
            MAXIMUM_BUILD_DERIVATION_INPUT_SOURCES,
            std::mem::size_of::<Vec<u8>>(),
            &self.budget,
        )?;
        let mut input_sources = Vec::with_capacity(input_count);
        for _ in 0..input_count {
            let (path, path_charge) = read_build_string(
                &mut self.input,
                MAXIMUM_WORKER_STORE_PATH_BYTES,
                &self.budget,
            )?;
            validate_build_store_path(&path)
                .map_err(|_| invalid("invalid BuildDerivation request"))?;
            if input_sources.iter().any(|v| v == &path) {
                return Err(invalid("invalid BuildDerivation request"));
            }
            string_charges.push(path_charge);
            input_sources.push(path);
        }

        let (platform, platform_charge) = read_build_string(
            &mut self.input,
            MAXIMUM_BUILD_DERIVATION_PLATFORM_BYTES,
            &self.budget,
        )?;
        if platform.is_empty() || platform.contains(&0) {
            return Err(invalid("invalid BuildDerivation request"));
        }
        let (builder, builder_charge) = read_build_string(
            &mut self.input,
            MAXIMUM_BUILD_DERIVATION_BUILDER_BYTES,
            &self.budget,
        )?;
        if builder.is_empty() || builder.contains(&0) {
            return Err(invalid("invalid BuildDerivation request"));
        }
        let (argument_count, argument_collection_charge) = read_build_count(
            &mut self.input,
            MAXIMUM_BUILD_DERIVATION_ARGUMENTS,
            std::mem::size_of::<Vec<u8>>(),
            &self.budget,
        )?;
        let mut arguments = Vec::with_capacity(argument_count);
        for _ in 0..argument_count {
            let (value, value_charge) = read_build_string(
                &mut self.input,
                MAXIMUM_BUILD_DERIVATION_ARGUMENT_BYTES,
                &self.budget,
            )?;
            if value.contains(&0) {
                return Err(invalid("invalid BuildDerivation request"));
            }
            string_charges.push(value_charge);
            arguments.push(value);
        }
        let (environment_count, environment_collection_charge) = read_build_count(
            &mut self.input,
            MAXIMUM_BUILD_DERIVATION_ENVIRONMENT,
            std::mem::size_of::<(Vec<u8>, Vec<u8>)>(),
            &self.budget,
        )?;
        let mut environment = Vec::with_capacity(environment_count);
        for _ in 0..environment_count {
            let (key, key_charge) = read_build_string(
                &mut self.input,
                MAXIMUM_BUILD_DERIVATION_ENVIRONMENT_KEY_BYTES,
                &self.budget,
            )?;
            if key.is_empty()
                || key.contains(&0)
                || key.contains(&b'=')
                || environment
                    .iter()
                    .any(|(existing_key, _)| existing_key == &key)
            {
                return Err(invalid("invalid BuildDerivation request"));
            }
            let (value, value_charge) = read_build_string(
                &mut self.input,
                MAXIMUM_BUILD_DERIVATION_ENVIRONMENT_VALUE_BYTES,
                &self.budget,
            )?;
            if value.contains(&0) {
                return Err(invalid("invalid BuildDerivation request"));
            }
            string_charges.push(key_charge);
            string_charges.push(value_charge);
            environment.push((key, value));
        }
        let build_mode = self
            .read_integer()
            .map_err(|_| invalid("invalid BuildDerivation request"))?;
        if build_mode != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid BuildDerivation request",
            ));
        }
        if self.input.has_unread_message_data() {
            return Err(invalid("invalid BuildDerivation request"));
        }
        self.input.complete_message();
        Ok(BuildDerivationRequest {
            drv_path,
            outputs: output_values,
            input_sources,
            platform,
            builder,
            arguments,
            environment,
            build_mode,
            _charges: BuildDerivationCharges {
                _collection_charges: vec![
                    output_collection_charge,
                    input_collection_charge,
                    argument_collection_charge,
                    environment_collection_charge,
                ],
                _string_charges: {
                    let mut charges = vec![drv_charge, platform_charge, builder_charge];
                    charges.append(&mut string_charges);
                    charges
                },
            },
        })
    }

    pub fn complete_set_options(&mut self) -> io::Result<()> {
        let span = tracing::info_span!("worker.set_options");
        let _entered = span.enter();
        for _ in 0..12 {
            self.read_integer()?;
        }

        let override_count = self.read_integer()?;
        if override_count > 256 {
            tracing::error!(
                event = "worker.set_options.rejected",
                reason = "override-count"
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "too many option overrides",
            ));
        }
        for _ in 0..override_count {
            self.discard_byte_string(16_384)?;
            self.discard_byte_string(16_384)?;
        }
        self.input.complete_message();
        Ok(())
    }

    pub fn into_inner(self) -> R {
        self.input
    }

    fn read_integer(&mut self) -> io::Result<u64> {
        read_worker_integer_from(&mut self.input)
    }

    fn read_strings(&mut self) -> io::Result<DecodedWorkerStrings> {
        read_worker_strings_from(&mut self.input, &self.budget)
    }

    fn discard_byte_string(&mut self, maximum_length: usize) -> io::Result<()> {
        let length = usize::try_from(self.read_integer()?).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "worker string exceeds limit")
        })?;
        if length > maximum_length {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "worker string exceeds limit",
            ));
        }
        let padding_length = (8 - length % 8) % 8;
        let framed_length = length + padding_length;
        let mut remaining = framed_length;
        let mut buffer = [0_u8; 4096];
        while remaining > 0 {
            let read_length = remaining.min(buffer.len());
            self.input.read_exact(&mut buffer[..read_length])?;
            if remaining == read_length
                && padding_length > 0
                && buffer[read_length - padding_length..read_length]
                    .iter()
                    .any(|byte| *byte != 0)
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "worker string padding is not zero",
                ));
            }
            remaining -= read_length;
        }
        Ok(())
    }
}

impl From<io::Error> for AddMultipleToStoreRequestError {
    fn from(error: io::Error) -> Self {
        Self(error.kind(), error.to_string())
    }
}

struct FramedReader<'a, R> {
    input: &'a mut R,
    remaining: u64,
    finished: bool,
}

impl<'a, R> FramedReader<'a, R> {
    fn new(input: &'a mut R) -> Self {
        Self {
            input,
            remaining: 0,
            finished: false,
        }
    }
}

impl<R: Read> Read for FramedReader<'_, R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() || self.finished {
            return Ok(0);
        }
        if self.remaining == 0 {
            self.remaining = read_worker_integer_from(self.input).map_err(|error| {
                if error.kind() == io::ErrorKind::UnexpectedEof {
                    io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "truncated AddMultipleToStore frame",
                    )
                } else {
                    error
                }
            })?;
            if self.remaining == 0 {
                self.finished = true;
                return Ok(0);
            }
        }
        let count = output
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        self.input.read_exact(&mut output[..count])?;
        self.remaining -= count as u64;
        Ok(count)
    }
}

fn read_bounded_count(
    input: &mut impl Read,
    maximum: usize,
) -> Result<usize, AddMultipleToStoreRequestError> {
    let value = read_worker_integer_from(input)?;
    usize::try_from(value)
        .ok()
        .filter(|value| *value <= maximum)
        .ok_or_else(|| {
            AddMultipleToStoreRequestError(
                io::ErrorKind::InvalidData,
                "AddMultipleToStore count exceeds limit".to_owned(),
            )
        })
}

fn read_add_multiple_string(
    input: &mut impl Read,
    maximum: usize,
    budget: &SessionAllocationBudget,
    charges: &mut Vec<SessionAllocationCharge>,
) -> Result<Vec<u8>, AddMultipleToStoreRequestError> {
    let (value, charge) = read_worker_byte_string_with_charge_from(input, maximum, budget)?;
    charges.push(charge);
    Ok(value)
}

fn read_optional_add_multiple_path(
    input: &mut impl Read,
    budget: &SessionAllocationBudget,
    charges: &mut Vec<SessionAllocationCharge>,
) -> Result<Option<Vec<u8>>, AddMultipleToStoreRequestError> {
    let value = read_add_multiple_string(input, MAXIMUM_WORKER_STORE_PATH_BYTES, budget, charges)?;
    if value.is_empty() {
        return Ok(None);
    }
    validate_store_path(&value)?;
    Ok(Some(value))
}

fn read_add_multiple_path_info(
    input: &mut impl Read,
    budget: &SessionAllocationBudget,
) -> Result<AddMultipleToStorePathInfo, AddMultipleToStoreRequestError> {
    let mut charges = Vec::new();
    let path =
        read_add_multiple_string(input, MAXIMUM_WORKER_STORE_PATH_BYTES, budget, &mut charges)?;
    validate_store_path(&path)?;
    let deriver = read_optional_add_multiple_path(input, budget, &mut charges)?;
    let nar_hash = read_add_multiple_string(
        input,
        MAXIMUM_ADD_MULTIPLE_TO_STORE_HASH_BYTES,
        budget,
        &mut charges,
    )?;
    let reference_count = read_bounded_count(input, MAXIMUM_ADD_MULTIPLE_TO_STORE_REFERENCES)?;
    let mut references = Vec::with_capacity(reference_count);
    let reference_bytes = reference_count
        .checked_mul(std::mem::size_of::<Vec<u8>>())
        .ok_or_else(|| {
            AddMultipleToStoreRequestError(
                io::ErrorKind::InvalidData,
                "AddMultipleToStore metadata exceeds limit".to_owned(),
            )
        })?;
    charges.push(budget.charge(reference_bytes).map_err(|_| {
        AddMultipleToStoreRequestError(
            io::ErrorKind::InvalidData,
            "AddMultipleToStore metadata exceeds limit".to_owned(),
        )
    })?);
    for _ in 0..reference_count {
        let reference =
            read_add_multiple_string(input, MAXIMUM_WORKER_STORE_PATH_BYTES, budget, &mut charges)?;
        validate_store_path(&reference)?;
        references.push(reference);
    }
    let registration_time = read_worker_integer_from(input)?;
    let nar_size = read_worker_integer_from(input)?;
    let ultimate = read_strict_worker_boolean(input, "ultimate")?;
    let signature_count = read_bounded_count(input, MAXIMUM_ADD_MULTIPLE_TO_STORE_SIGNATURES)?;
    let mut signatures = Vec::with_capacity(signature_count);
    let signature_bytes = signature_count
        .checked_mul(std::mem::size_of::<Vec<u8>>())
        .ok_or_else(|| {
            AddMultipleToStoreRequestError(
                io::ErrorKind::InvalidData,
                "AddMultipleToStore metadata exceeds limit".to_owned(),
            )
        })?;
    charges.push(budget.charge(signature_bytes).map_err(|_| {
        AddMultipleToStoreRequestError(
            io::ErrorKind::InvalidData,
            "AddMultipleToStore metadata exceeds limit".to_owned(),
        )
    })?);
    for _ in 0..signature_count {
        signatures.push(read_add_multiple_string(
            input,
            MAXIMUM_ADD_MULTIPLE_TO_STORE_SIGNATURE_BYTES,
            budget,
            &mut charges,
        )?);
    }
    let content_address = {
        let value = read_add_multiple_string(
            input,
            MAXIMUM_ADD_MULTIPLE_TO_STORE_CONTENT_ADDRESS_BYTES,
            budget,
            &mut charges,
        )?;
        (!value.is_empty()).then_some(value)
    };
    Ok(AddMultipleToStorePathInfo {
        path,
        deriver,
        nar_hash,
        references,
        registration_time,
        nar_size,
        ultimate,
        signatures,
        content_address,
        _charges: charges,
    })
}

fn invalid_query_missing_request() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid QueryMissing request")
}

fn validate_derived_path(target: &[u8]) -> io::Result<()> {
    let path = match target.iter().position(|byte| *byte == b'!') {
        Some(separator) => {
            let (path, suffix) = target.split_at(separator);
            let outputs = &suffix[1..];
            if outputs.is_empty()
                || outputs.split(|byte| *byte == b',').any(|output| {
                    output.is_empty()
                        || (output != b"*"
                            && !output.iter().all(|byte| {
                                byte.is_ascii_alphanumeric()
                                    || matches!(byte, b'+' | b'-' | b'.' | b'_')
                            }))
                })
            {
                return Err(invalid_query_missing_request());
            }
            path
        }
        None => target,
    };
    validate_store_path(path).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("invalid QueryMissing derived path: {error}"),
        )
    })
}

fn validate_build_store_path(path: &[u8]) -> io::Result<()> {
    validate_store_path(path)?;
    if !path.starts_with(NIX_STORE_DIRECTORY) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid store path",
        ));
    }
    Ok(())
}

fn validate_build_output_hash(hash_algorithm: &[u8], hash: &[u8]) -> io::Result<()> {
    if hash_algorithm.is_empty() && hash.is_empty() {
        return Ok(());
    }
    let algorithm = hash_algorithm.strip_prefix(b"r:").unwrap_or(hash_algorithm);
    let expected_hex_bytes = match algorithm {
        b"md5" => 32,
        b"sha1" => 40,
        b"sha256" => 64,
        b"sha512" => 128,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported BuildDerivation output hash algorithm",
            ));
        }
    };
    if hash.len() != expected_hex_bytes
        || !hash
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid BuildDerivation output hash",
        ));
    }
    Ok(())
}

fn read_build_count(
    input: &mut impl Read,
    maximum: usize,
    element_size: usize,
    budget: &SessionAllocationBudget,
) -> io::Result<(usize, SessionAllocationCharge)> {
    let count = usize::try_from(read_worker_integer_from(input)?).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid BuildDerivation request",
        )
    })?;
    if count > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid BuildDerivation request",
        ));
    }
    let bytes = count.checked_mul(element_size).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid BuildDerivation request",
        )
    })?;
    let charge = budget.charge(bytes).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid BuildDerivation request",
        )
    })?;
    Ok((count, charge))
}

fn read_build_string(
    input: &mut impl Read,
    maximum: usize,
    budget: &SessionAllocationBudget,
) -> io::Result<(Vec<u8>, SessionAllocationCharge)> {
    read_worker_byte_string_with_charge_from(input, maximum, budget)
}

fn read_strict_worker_boolean(input: &mut impl Read, name: &str) -> io::Result<bool> {
    match read_worker_integer_from(input)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid AddMultipleToStore {name} boolean"),
        )),
    }
}

fn read_worker_integer_from(input: &mut impl Read) -> io::Result<u64> {
    let mut bytes = [0; 8];
    input.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn negotiate_worker_version_with_budget(
    client_version: WorkerVersion,
    client_features: &[String],
    server_features: &[String],
    budget: &SessionAllocationBudget,
) -> Result<NegotiatedWorkerVersion, ProtocolError> {
    let version = client_version.min(LATEST_WORKER_VERSION);
    if version < MINIMUM_WORKER_VERSION {
        return Err(ProtocolError::VersionMismatch);
    }

    let feature_count = client_features
        .iter()
        .filter(|feature| server_features.contains(feature))
        .count();
    let feature_capacity = feature_count
        .checked_mul(std::mem::size_of::<String>())
        .ok_or(ProtocolError::SizeLimit)?;
    let feature_charge = budget.charge(feature_capacity)?;
    let mut features = Vec::with_capacity(feature_count);
    let mut feature_charges = Vec::with_capacity(feature_count);
    for feature in client_features
        .iter()
        .filter(|feature| server_features.contains(feature))
    {
        let charge = budget.charge(feature.capacity())?;
        features.push(feature.clone());
        feature_charges.push(charge);
    }
    let metadata_charge = SessionAllocationCharges {
        _collection_charge: feature_charge,
        _value_charges: feature_charges,
    };
    Ok(NegotiatedWorkerVersion {
        version,
        features,
        _feature_charge: Some(metadata_charge),
    })
}

#[derive(Debug)]
struct SessionAllocationCharges {
    _collection_charge: SessionAllocationCharge,
    _value_charges: Vec<SessionAllocationCharge>,
}

#[derive(Debug)]
struct DecodedWorkerStrings {
    values: Vec<String>,
    _collection_charge: SessionAllocationCharge,
    _value_charges: Vec<SessionAllocationCharge>,
}

fn discard_worker_strings(
    input: &mut impl Read,
    budget: &SessionAllocationBudget,
) -> io::Result<()> {
    let strings = read_worker_strings_from(input, budget)?;
    drop(strings);
    Ok(())
}

fn read_worker_strings_from(
    input: &mut impl Read,
    budget: &SessionAllocationBudget,
) -> io::Result<DecodedWorkerStrings> {
    let count = usize::try_from(read_worker_integer_from(input)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "too many worker features"))?;
    if count > MAXIMUM_HANDSHAKE_FEATURES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "too many worker features",
        ));
    }
    let collection_capacity = count
        .checked_mul(std::mem::size_of::<String>())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "worker metadata exceeds session limit",
            )
        })?;
    let collection_charge = budget.charge(collection_capacity).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "worker metadata exceeds session limit",
        )
    })?;
    let mut values = Vec::with_capacity(count);
    let mut value_charges = Vec::with_capacity(count);
    for _ in 0..count {
        let (feature, charge) = read_worker_byte_string_with_charge_from(
            input,
            MAXIMUM_HANDSHAKE_FEATURE_LENGTH,
            budget,
        )?;
        let feature = String::from_utf8(feature).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "worker feature is not UTF-8")
        })?;
        values.push(feature);
        value_charges.push(charge);
    }
    Ok(DecodedWorkerStrings {
        values,
        _collection_charge: collection_charge,
        _value_charges: value_charges,
    })
}

fn read_worker_byte_string_with_charge_from(
    input: &mut impl Read,
    maximum_length: usize,
    budget: &SessionAllocationBudget,
) -> io::Result<(Vec<u8>, SessionAllocationCharge)> {
    let length = usize::try_from(read_worker_integer_from(input)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "worker string exceeds limit"))?;
    if length > maximum_length {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worker string exceeds limit",
        ));
    }
    let padding_length = (8 - length % 8) % 8;
    let framed_length = length
        .checked_add(padding_length)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "worker string exceeds limit"))?;
    let charge = budget.charge(framed_length).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "worker metadata exceeds session limit",
        )
    })?;
    let mut framed = vec![0; framed_length];
    input.read_exact(&mut framed)?;
    if framed[length..].iter().any(|byte| *byte != 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "worker string padding is not zero",
        ));
    }
    framed.truncate(length);
    Ok((framed, charge))
}

#[derive(Debug)]
pub struct StorePathRequest {
    path: Vec<u8>,
    _charge: SessionAllocationCharge,
}

impl StorePathRequest {
    pub fn path(&self) -> &[u8] {
        &self.path
    }
}

#[derive(Debug)]
pub struct QueryMissingRequest {
    targets: Vec<Vec<u8>>,
    _collection_charge: SessionAllocationCharge,
    _value_charges: Vec<SessionAllocationCharge>,
}

impl QueryMissingRequest {
    pub fn targets(&self) -> &[Vec<u8>] {
        &self.targets
    }
}

#[derive(Debug)]
pub struct BuildPathsWithResultsRequest {
    targets: Vec<Vec<u8>>,
    build_mode: u64,
    _collection_charge: SessionAllocationCharge,
    _value_charges: Vec<SessionAllocationCharge>,
}

impl BuildPathsWithResultsRequest {
    pub fn targets(&self) -> &[Vec<u8>] {
        &self.targets
    }

    pub fn build_mode(&self) -> u64 {
        self.build_mode
    }
}

#[derive(Debug)]
pub struct QueryValidPathsRequest {
    paths: Vec<Vec<u8>>,
    substitute: bool,
    _collection_charge: SessionAllocationCharge,
    _value_charges: Vec<SessionAllocationCharge>,
}

impl QueryValidPathsRequest {
    pub fn paths(&self) -> &[Vec<u8>] {
        &self.paths
    }

    pub fn substitute(&self) -> bool {
        self.substitute
    }
}

mod client;

use client::validate_store_path;
pub use client::*;

pub fn write_build_paths_with_results_success_response<'a>(
    output: &mut impl Write,
    version: WorkerVersion,
    results: impl IntoIterator<Item = (&'a [u8], bool)>,
) -> io::Result<()> {
    let results = results.into_iter().collect::<Vec<_>>();
    if results.len() > MAXIMUM_QUERY_VALID_PATHS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many BuildPathsWithResults results",
        ));
    }
    output.write_all(&STDERR_LAST.to_le_bytes())?;
    write_worker_integer_to(output, results.len() as u64)?;
    for (target, already_valid) in results {
        validate_derived_path(target)?;
        write_worker_byte_string_to(output, target)?;
        write_build_result_success(output, version, already_valid)?;
    }
    output.flush()
}

pub fn write_build_derivation_success_response(
    output: &mut impl Write,
    version: WorkerVersion,
    already_valid: bool,
) -> io::Result<()> {
    output.write_all(&STDERR_LAST.to_le_bytes())?;
    write_build_result_success(output, version, already_valid)?;
    output.flush()
}

fn write_build_result_success(
    output: &mut impl Write,
    version: WorkerVersion,
    already_valid: bool,
) -> io::Result<()> {
    write_worker_integer_to(output, if already_valid { 2 } else { 0 })?;
    write_worker_byte_string_to(output, b"")?;
    if version >= WorkerVersion::new(1, 29) {
        for value in [0_u64; 4] {
            write_worker_integer_to(output, value)?;
        }
    }
    if version >= WorkerVersion::new(1, 37) {
        write_worker_integer_to(output, 0)?;
        write_worker_integer_to(output, 0)?;
    }
    if version >= WorkerVersion::new(1, 28) {
        write_worker_integer_to(output, 0)?;
    }
    Ok(())
}

pub struct PathInfoResponse<'a> {
    pub deriver: Option<&'a [u8]>,
    pub nar_hash_hex: &'a str,
    pub references: &'a [Vec<u8>],
    pub registration_time: u64,
    pub nar_size: u64,
    pub ultimate: bool,
    pub signatures: &'a [String],
    pub content_address: Option<&'a str>,
}

pub fn write_query_path_info_response(
    output: &mut impl Write,
    version: WorkerVersion,
    info: Option<PathInfoResponse<'_>>,
) -> io::Result<()> {
    let Some(info) = info else {
        return write_worker_integer_to(output, 0);
    };
    write_worker_integer_to(output, 1)?;
    write_worker_byte_string_to(output, info.deriver.unwrap_or_default())?;
    if info.nar_hash_hex.len() != 64
        || !info
            .nar_hash_hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid path info NAR hash",
        ));
    }
    write_worker_byte_string_to(output, info.nar_hash_hex.as_bytes())?;
    write_worker_integer_to(output, info.references.len() as u64)?;
    for reference in info.references {
        validate_store_path(reference).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid path info reference")
        })?;
        write_worker_byte_string_to(output, reference)?;
    }
    write_worker_integer_to(output, info.registration_time)?;
    write_worker_integer_to(output, info.nar_size)?;
    if version >= WorkerVersion::new(1, 16) {
        write_worker_integer_to(output, u64::from(info.ultimate))?;
        write_worker_integer_to(output, info.signatures.len() as u64)?;
        for signature in info.signatures {
            write_worker_byte_string_to(output, signature.as_bytes())?;
        }
        write_worker_byte_string_to(output, info.content_address.unwrap_or_default().as_bytes())?;
    }
    Ok(())
}

pub fn write_query_missing_response(
    output: &mut impl Write,
    will_build: impl IntoIterator<Item = impl AsRef<[u8]>>,
    will_substitute: impl IntoIterator<Item = impl AsRef<[u8]>>,
    unknown: impl IntoIterator<Item = impl AsRef<[u8]>>,
    download_size: u64,
    nar_size: u64,
) -> io::Result<()> {
    write_query_missing_path_set(output, will_build)?;
    write_query_missing_path_set(output, will_substitute)?;
    write_query_missing_path_set(output, unknown)?;
    write_worker_integer_to(output, download_size)?;
    write_worker_integer_to(output, nar_size)
}

fn write_query_missing_path_set(
    output: &mut impl Write,
    paths: impl IntoIterator<Item = impl AsRef<[u8]>>,
) -> io::Result<()> {
    let mut paths = paths
        .into_iter()
        .map(|path| path.as_ref().to_vec())
        .collect::<Vec<_>>();
    if paths.len() > MAXIMUM_QUERY_VALID_PATHS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many QueryMissing results",
        ));
    }
    for path in &paths {
        validate_store_path(path).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid QueryMissing response")
        })?;
    }
    paths.sort();
    paths.dedup();
    write_worker_integer_to(output, paths.len() as u64)?;
    for path in paths {
        write_worker_byte_string_to(output, &path)?;
    }
    Ok(())
}

pub fn write_query_valid_paths_response(
    output: &mut impl Write,
    paths: impl IntoIterator<Item = impl AsRef<[u8]>>,
) -> io::Result<()> {
    let mut paths = paths
        .into_iter()
        .map(|path| path.as_ref().to_vec())
        .collect::<Vec<_>>();
    if paths.len() > MAXIMUM_QUERY_VALID_PATHS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "too many QueryValidPaths results",
        ));
    }
    for path in &paths {
        validate_store_path(path).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid QueryValidPaths response",
            )
        })?;
    }
    paths.sort();
    if paths.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "duplicate QueryValidPaths result",
        ));
    }
    write_worker_integer_to(output, paths.len() as u64)?;
    for path in paths {
        write_worker_byte_string_to(output, &path)?;
    }
    Ok(())
}

fn write_worker_integer_to(output: &mut impl Write, value: u64) -> io::Result<()> {
    output.write_all(&value.to_le_bytes())
}

fn write_worker_byte_string_to(output: &mut impl Write, value: &[u8]) -> io::Result<()> {
    write_worker_integer_to(output, value.len() as u64)?;
    output.write_all(value)?;
    output.write_all(&[0; 7][..(8 - value.len() % 8) % 8])
}

fn write_worker_strings_to(output: &mut impl Write, values: &[String]) -> io::Result<()> {
    write_worker_integer_to(output, values.len() as u64)?;
    for value in values {
        write_worker_byte_string_to(output, value.as_bytes())?;
    }
    Ok(())
}

pub fn write_worker_integer(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

pub fn write_server_worker_magic(output: &mut Vec<u8>) {
    write_worker_integer(output, SERVER_WORKER_MAGIC);
}

pub fn read_worker_byte_string(
    input: &mut &[u8],
    maximum_length: usize,
) -> Result<Vec<u8>, ProtocolError> {
    let length = read_worker_integer(input)?;
    let length = usize::try_from(length).map_err(|_| ProtocolError::SizeLimit)?;
    if length > maximum_length {
        return Err(ProtocolError::SizeLimit);
    }

    let padding_length = (8 - length % 8) % 8;
    let framed_length = length
        .checked_add(padding_length)
        .ok_or(ProtocolError::SizeLimit)?;
    if input.len() < framed_length {
        return Err(ProtocolError::Truncated);
    }

    let (framed, remaining) = input.split_at(framed_length);
    let (payload, padding) = framed.split_at(length);
    if padding.iter().any(|byte| *byte != 0) {
        return Err(ProtocolError::InternalFailure);
    }

    *input = remaining;
    Ok(payload.to_vec())
}

pub fn write_worker_byte_string(output: &mut Vec<u8>, value: &[u8]) {
    write_worker_integer(output, value.len() as u64);
    output.extend_from_slice(value);
    output.resize(output.len() + (8 - value.len() % 8) % 8, 0);
}

#[cfg(test)]
mod tests;
