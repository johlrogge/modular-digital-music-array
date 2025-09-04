default:
    just --list

# watch progman and run check, test, build and clippy when files change
[group('build')]
watch:
    cargo watch -x check -x test -x build -x clippy

# just build progman
[group('build')]
build:
    cargo build
    
# build progman for windows
[group('build')]
build-for-windows:
    cargo build --target x86_64-pc-windows-gnu

# generate the open_api client for atum (manually ignore warnings afterwards)
[group('generate')]
generate-atum-api:
    openapi-generator generate \
      -i openapi/AtumAPI.json \
      -g rust \
      -o components/atum_rest_client \
      --package-name atum_rest_client \
      --git-user-id electrolux \
      --git-repo-id dxp-progman

# build and start the atum test server
[group('test')]
atum-test-server: build
    target/debug/atum_test_server&

# start the atum test server, start repoman with the test server as a source
[group('test')]
repoman: build atum-test-server
    target/debug/dxp-repoman --atum-url http://localhost:3030
    
    
