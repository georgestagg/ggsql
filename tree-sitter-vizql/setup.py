from setuptools import setup, Extension
import os

# Get the path to the C source files
c_src_path = os.path.join("src", "parser.c")
include_dirs = ["src"]

# Define the extension module
vizql_extension = Extension(
    "tree_sitter_vizql.binding",
    sources=[
        "bindings/python/tree_sitter_vizql/binding.c",
        c_src_path,
    ],
    include_dirs=include_dirs,
    extra_compile_args=[
        "-std=c99",
    ] if os.name != "nt" else [],
)

setup(
    ext_modules=[vizql_extension],
    package_dir={"": "bindings/python"},
)