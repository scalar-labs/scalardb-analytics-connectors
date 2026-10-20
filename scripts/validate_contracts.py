#!/usr/bin/env python3
"""Validate the connector contract inventory and skeleton artifacts."""

from __future__ import annotations

import json
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parent.parent
CONTRACT_ROOT = REPOSITORY_ROOT / "contract"
REQUIRED_ARTIFACTS = {
    ("c-header", "connector_api.h"),
    ("arrow-schema", "requests/metadata-request.arrow.json"),
    ("arrow-schema", "requests/metadata-response.arrow.json"),
    ("json-schema", "declarations/capability.schema.json"),
    ("json-schema", "declarations/static-information.schema.json"),
    ("substrait-contract", "substrait/contract.json"),
    ("arrow-extensions", "analytics-types/arrow-extensions.json"),
    ("type-mapping", "type-mappings/postgresql.json"),
    ("type-mapping", "type-mappings/mysql.json"),
}


def load_json(path: Path) -> object:
    with path.open(encoding="utf-8") as source:
        return json.load(source)


def require_mapping(value: object, path: Path) -> dict[str, object]:
    if not isinstance(value, dict):
        raise ValueError(f"{path}: expected a JSON object")
    return value


def validate_arrow_extensions(document: dict[str, object], path: Path) -> None:
    if document.get("extension_name_prefix") != "scalardb.analytics.":
        raise ValueError(f"{path}: invalid extension_name_prefix")

    arrow_extensions = document.get("arrow_extensions")
    if not isinstance(arrow_extensions, list):
        raise ValueError(f"{path}: arrow_extensions must be an array")

    observed: dict[str, str] = {}
    for arrow_extension in arrow_extensions:
        if not isinstance(arrow_extension, dict):
            raise ValueError(f"{path}: each Arrow extension must be an object")
        name = arrow_extension.get("name")
        parameters = arrow_extension.get("parameters")
        storage_type = arrow_extension.get("storage_type")
        value_representation = arrow_extension.get("value_representation")
        if not isinstance(name, str) or not isinstance(storage_type, str):
            raise ValueError(f"{path}: extension name and storage type are required")
        if name in observed:
            raise ValueError(f"{path}: duplicate Arrow extension {name}")
        if not isinstance(parameters, list) or not isinstance(value_representation, dict):
            raise ValueError(f"{path}: {name} requires parameters and value_representation")
        if not isinstance(value_representation.get("form"), str) or not isinstance(
            value_representation.get("equality"), str
        ):
            raise ValueError(f"{path}: {name} requires value form and equality rules")
        observed[name] = storage_type

    expected = {
        "uuid": "utf8",
        "json": "utf8",
        "enum": "utf8",
        "year": "int16",
        "timetz": "utf8",
    }
    if observed != expected:
        raise ValueError(
            f"{path}: Analytics Arrow extensions must be {expected}, found {observed}"
        )

    enum_entry = next(
        entry for entry in arrow_extensions
        if isinstance(entry, dict) and entry["name"] == "enum"
    )
    expected_enum_parameters = [
        {
            "name": "members",
            "type": "string-array",
        }
    ]
    if enum_entry["parameters"] != expected_enum_parameters:
        raise ValueError(f"{path}: enum parameter definition is invalid")

    for arrow_extension in arrow_extensions:
        if arrow_extension["name"] != "enum" and arrow_extension["parameters"]:
            raise ValueError(f"{path}: {arrow_extension['name']} must not have parameters")


def main() -> None:
    manifest_path = CONTRACT_ROOT / "manifest.json"
    manifest = require_mapping(load_json(manifest_path), manifest_path)
    artifacts = manifest.get("artifacts")
    if not isinstance(artifacts, list) or not artifacts:
        raise ValueError(f"{manifest_path}: artifacts must be a non-empty array")

    observed_artifacts: set[tuple[str, str]] = set()
    observed_paths: set[Path] = set()

    for entry in artifacts:
        if not isinstance(entry, dict):
            raise ValueError(f"{manifest_path}: each artifact must be an object")
        kind = entry.get("kind")
        relative_path = entry.get("path")
        if not isinstance(kind, str) or not isinstance(relative_path, str):
            raise ValueError(f"{manifest_path}: artifact kind and path must be strings")

        artifact_path = (CONTRACT_ROOT / relative_path).resolve()
        if CONTRACT_ROOT.resolve() not in artifact_path.parents:
            raise ValueError(f"{relative_path}: artifact escapes contract directory")
        if artifact_path in observed_paths:
            raise ValueError(f"{relative_path}: duplicate artifact path")
        if not artifact_path.is_file():
            raise ValueError(f"{relative_path}: artifact does not exist")

        observed_artifacts.add((kind, relative_path))
        observed_paths.add(artifact_path)
        if artifact_path.suffix == ".json":
            document = require_mapping(load_json(artifact_path), artifact_path)
            if kind == "arrow-schema" and not isinstance(document.get("fields"), list):
                raise ValueError(f"{relative_path}: fields must be an array")
            if kind == "json-schema" and "$schema" not in document:
                raise ValueError(f"{relative_path}: missing $schema")
            if kind == "arrow-extensions":
                validate_arrow_extensions(document, artifact_path)
            if kind == "type-mapping" and not isinstance(document.get("rules"), list):
                raise ValueError(f"{relative_path}: rules must be an array")
            if kind == "substrait-contract" and "accepted_version" not in document:
                raise ValueError(f"{relative_path}: missing accepted_version")

    missing_artifacts = REQUIRED_ARTIFACTS - observed_artifacts
    if missing_artifacts:
        raise ValueError(f"missing required artifacts: {sorted(missing_artifacts)}")

    header = (CONTRACT_ROOT / "connector_api.h").read_text(encoding="utf-8")
    required_header_tokens = (
        "SCALARDB_ANALYTICS_CONNECTOR_API_H",
        "SCALARDB_ANALYTICS_CONNECTOR_CONTRACT_VERSION_MAJOR",
        'extern "C"',
    )
    for token in required_header_tokens:
        if token not in header:
            raise ValueError(f"connector_api.h: missing {token}")

    print(f"validated {len(artifacts)} contract artifacts")


if __name__ == "__main__":
    main()
