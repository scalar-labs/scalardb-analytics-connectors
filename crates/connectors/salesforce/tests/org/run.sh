#!/bin/sh
# Prepares a Salesforce org for the connector's org tests and runs them
# against it, through an org the Salesforce CLI has logged in to.
#
# Usage:
#   run.sh prepare <org alias> [bulk records]
#   run.sh test <org alias> [cargo test arguments...]
#
# prepare deploys sfdx/force-app, assigns the ConnectorTest permission set to
# the logged-in user, upserts sfdx/data, upserts the given number of records
# (default 10000) into ConnectorTestBulk__c, and creates one Task for the
# field types only standard objects have. Running it again leaves the same
# org.
#
# test reads the refresh token, client ID, and client secret of the CLI's
# login and runs `cargo test --test org` with them as the connector's
# options, without printing them. When SALESFORCE_TEST_JWT_CLIENT_ID,
# SALESFORCE_TEST_JWT_USERNAME, and SALESFORCE_TEST_JWT_PRIVATE_KEY_FILE are
# set, it runs the tests again with the jwt_bearer flow.
#
# Log in first, which opens a browser:
#   sf org login web --instance-url <My Domain URL> --alias <org alias>
#
# Requires sf and jq on PATH, as the development shell provides.
set -eu

usage() {
	echo "usage: run.sh prepare <org alias> [bulk records]" >&2
	echo "       run.sh test <org alias> [cargo test arguments...]" >&2
	exit 2
}

[ $# -ge 2 ] || usage
command=$1
org=$2
shift 2
here=$(cd "$(dirname "$0")" && pwd)

prepare() {
	records=${1:-10000}
	cd "$here/sfdx"
	sf project deploy start --source-dir force-app --target-org "$org" --wait 10
	# Assigning a permission set the user already has is an error; anything
	# else stops the script.
	if ! output=$(sf org assign permset --name ConnectorTest --target-org "$org" 2>&1); then
		case $output in
		*"Duplicate PermissionSetAssignment"*) ;;
		*) echo "$output" >&2; exit 1 ;;
		esac
	fi
	bulk=$(mktemp -d)
	trap 'rm -rf "$bulk"' EXIT
	{
		echo "Seq__c,Name"
		i=1
		while [ "$i" -le "$records" ]; do
			echo "$i,record $i"
			i=$((i + 1))
		done
	} > "$bulk/ConnectorTestBulk__c.csv"
	sf data upsert bulk --sobject ConnectorTestBulk__c --file "$bulk/ConnectorTestBulk__c.csv" \
		--external-id Seq__c --wait 30 --target-org "$org"
	# Its min record refers to the ConnectorTestBulk__c record whose Seq__c is 1.
	sf data upsert bulk --sobject ConnectorTest__c --file data/ConnectorTest__c.csv \
		--external-id Key__c --wait 10 --target-org "$org"
	standard_records
}

# Prints the Id of the first record a SOQL query returns, or nothing.
first_id() {
	sf data query --target-org "$org" --json --query "$1" | jq -r '.result.records[0].Id // empty'
}

# Creates, unless it exists, a Task, whose fields have the types combobox and
# int that a custom object cannot have. The tests select it by its number,
# with a filter that runs remotely; the values must match
# tests/org/standard.rs.
standard_records() {
	task=$(first_id "SELECT Id FROM Task WHERE Subject = 'Connector Test Task'")
	if [ -z "$task" ]; then
		sf data create record --sobject Task --target-org "$org" \
			--values "Subject='Connector Test Task' CallDurationInSeconds=87654321"
	else
		sf data update record --sobject Task --record-id "$task" --target-org "$org" \
			--values "CallDurationInSeconds=87654321"
	fi
}

run_tests() {
	# The SFDX auth URL is force://<client ID>:<client secret>:<refresh
	# token>@<instance>; the CLI's own client has no secret.
	auth=$(sf org auth show-sfdx-auth-url --target-org "$org" --json | jq -r .result.sfdxAuthUrl)
	credentials=${auth#force://}
	instance=https://${credentials##*@}
	credentials=${credentials%@*}
	client_id=${credentials%%:*}
	credentials=${credentials#*:}
	client_secret=${credentials%%:*}
	refresh_token=${credentials#*:}
	cd "$here/../../../../.."

	SALESFORCE_TEST_LOGIN_URL=$instance \
		SALESFORCE_TEST_AUTH_FLOW=refresh_token \
		SALESFORCE_TEST_CLIENT_ID=$client_id \
		SALESFORCE_TEST_CLIENT_SECRET=$client_secret \
		SALESFORCE_TEST_REFRESH_TOKEN=$refresh_token \
		cargo test -p connector-salesforce --test org "$@"

	if [ -n "${SALESFORCE_TEST_JWT_CLIENT_ID:-}" ]; then
		# The assertion's audience is the login server of the org's kind,
		# not its My Domain URL.
		case $instance in
		*.sandbox.my.salesforce.com) audience=https://test.salesforce.com ;;
		*) audience=https://login.salesforce.com ;;
		esac
		SALESFORCE_TEST_LOGIN_URL=$instance \
			SALESFORCE_TEST_AUTH_FLOW=jwt_bearer \
			SALESFORCE_TEST_CLIENT_ID=$SALESFORCE_TEST_JWT_CLIENT_ID \
			SALESFORCE_TEST_CLIENT_SECRET= \
			SALESFORCE_TEST_REFRESH_TOKEN= \
			SALESFORCE_TEST_USERNAME=${SALESFORCE_TEST_JWT_USERNAME:?} \
			SALESFORCE_TEST_PRIVATE_KEY=$(cat "${SALESFORCE_TEST_JWT_PRIVATE_KEY_FILE:?}") \
			SALESFORCE_TEST_JWT_AUDIENCE=$audience \
			cargo test -p connector-salesforce --test org "$@"
	fi
}

case $command in
prepare) prepare "$@" ;;
test) run_tests "$@" ;;
*) usage ;;
esac
