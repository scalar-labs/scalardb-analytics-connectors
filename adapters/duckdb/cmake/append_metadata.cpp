// Appends the metadata footer DuckDB checks when it loads an extension: a
// WebAssembly custom-section header, eight 32-byte fields in reverse order,
// and a 256-byte signature, left empty for an unsigned extension. The layout
// follows DuckDB's scripts/append_metadata.cmake.
//
// Usage: append_metadata <extension> <platform> <duckdb version> <extension version>
#include <fstream>
#include <iostream>
#include <string>

int main(int argc, char **argv) {
	if (argc != 5) {
		std::cerr << "usage: append_metadata <extension> <platform> <duckdb version> <extension version>\n";
		return 2;
	}
	auto field = [](const std::string &value) {
		auto padded = value.substr(0, 32);
		padded.resize(32, '\0');
		return padded;
	};
	// Section id 0, its length 531 as LEB128, the name's length 16, the name,
	// and the payload's length 512 as LEB128.
	std::string footer("\x00\x93\x04\x10", 4);
	footer += "duckdb_signature";
	footer += std::string("\x80\x04", 2);
	// Fields 8 through 1: three unused, the ABI type, the extension version,
	// the DuckDB version, the platform, and the magic value.
	footer += field("") + field("") + field("") + field("CPP") + field(argv[4]) + field(argv[3]) + field(argv[2]) +
	          field("4");
	footer += std::string(256, '\0');
	std::ofstream extension(argv[1], std::ios::binary | std::ios::app);
	extension << footer;
	return extension ? 0 : 1;
}
