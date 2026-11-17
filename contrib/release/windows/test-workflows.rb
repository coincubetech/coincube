#!/usr/bin/env ruby
# Structural regression checks use Ruby's standard-library YAML parser.
require 'yaml'

root = File.expand_path('../../..', __dir__)
checks = 0
check = lambda do |condition, message|
  abort message unless condition
  checks += 1
end

%w[releases nightly].each do |workflow|
  data = YAML.load_file(File.join(root, '.github/workflows', "#{workflow}.yml"))
  jobs = data.fetch('jobs')
  job = jobs.fetch('windows')
  prefix = "#{workflow}: "
  check.call(data.fetch('permissions').fetch('id-token', 'none') == 'none', prefix + 'global OIDC permission')
  check.call(job.fetch('permissions') == { 'contents' => 'read', 'id-token' => 'write' }, prefix + 'signer permissions')
  environment = workflow == 'releases' ? 'windows-release-signing' : 'windows-nightly-signing'
  check.call(job.fetch('environment') == environment, prefix + 'signing environment')
  expected_guard = if workflow == 'releases'
    "github.repository == 'coincubetech/coincube' && startsWith(github.ref, 'refs/tags/v') && (github.event_name == 'push' || github.event_name == 'workflow_dispatch')"
  else
    "github.repository == 'coincubetech/coincube' && github.ref == 'refs/heads/master' && needs.check.outputs.should_run == 'true' && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')"
  end
  check.call(job.fetch('if') == expected_guard, prefix + 'trusted source/event guard')
  jobs.each do |name, other|
    next if name == 'windows'
    check.call(other.fetch('permissions', {}).fetch('id-token', 'none') != 'write', prefix + "OIDC leaked to #{name}")
    platforms = other.fetch('strategy', {}).fetch('matrix', {}).fetch('include', [])
    check.call(platforms.none? { |leg| leg['platform'] == 'windows' }, prefix + 'unsigned Windows matrix leg')
  end
  steps = job.fetch('steps')
  names = steps.map { |step| step['name'] }
  ordered = [
    'Require Windows signing configuration', 'Build Windows binaries', 'Azure login (OIDC)',
    'Sign Windows executables', 'Verify executable signatures before packaging', 'Package Windows MSI',
    'Sign Windows MSI', 'Verify MSI signature before extraction', 'Verify Spark bridge is in the MSI',
    workflow == 'releases' ? 'Stage Windows MSI' : 'Upload Windows artifact'
  ]
  positions = ordered.map { |name| names.index(name) }
  check.call(positions.none?(&:nil?) && positions == positions.sort, prefix + 'sign/verify/package/upload order')
  check.call(!job['continue-on-error'] && steps.none? { |step| step['continue-on-error'] }, prefix + 'failure bypass')
  required = steps.find { |step| step['name'] == ordered.first }.fetch('env')
  %w[AZURE_TENANT_ID AZURE_CLIENT_ID AZURE_SUBSCRIPTION_ID AZURE_ENDPOINT AZURE_CODE_SIGNING_NAME AZURE_CERT_PROFILE_NAME].each do |name|
    check.call(required[name] == "${{ vars.#{name} }}", prefix + "missing configuration #{name}")
  end
  signers = steps.select { |step| step.fetch('uses', '').start_with?('azure/artifact-signing-action@') }
  check.call(signers.length == 2, prefix + 'must sign payload and installer')
  signers.each do |step|
    check.call(step['uses'].match?(/@[a-f0-9]{40}$/), prefix + 'unpinned signing action')
    inputs = step.fetch('with')
    check.call(inputs['file-digest'] == 'SHA256' && inputs['timestamp-digest'] == 'SHA256' &&
      inputs['timestamp-rfc3161'] == 'http://timestamp.acs.microsoft.com', prefix + 'digest/timestamp configuration')
    check.call(inputs['cache-dependencies'] == false && inputs['exclude-environment-credential'] == true,
      prefix + 'cached signer or standing credential fallback')
    check.call(inputs.keys.none? { |key| key.start_with?('azure-') }, prefix + 'standing Azure credentials')
  end
  binaries = signers.first.fetch('with').fetch('files').lines.map(&:strip)
  expected = %w[coincube.exe coincube-spark-bridge.exe coincube-mcp.exe].map do |name|
    "${{ github.workspace }}\\target\\minimal\\#{name}"
  end
  check.call(binaries == expected, prefix + 'all three exact payload paths')
  verify = steps.find { |step| step['name'] == 'Verify executable signatures before packaging' }.fetch('run')
  expected.each { |path| check.call(verify.include?(path.split('\\').last), prefix + "unverified #{path}") }
  extracted = steps.find { |step| step['name'] == 'Verify Spark bridge is in the MSI' }.fetch('run')
  check.call(extracted.index('Assert-WindowsMsiPayload') < extracted.index('cargo test'), prefix + 'execute before verifying extracted payload')
  login = steps.find { |step| step['name'] == 'Azure login (OIDC)' }
  check.call(login['uses'].match?(/@[a-f0-9]{40}$/) && !login.fetch('with').key?('creds'), prefix + 'OIDC login pin/credentials')
  if workflow == 'releases'
    check.call(jobs.fetch('checksums').fetch('needs').include?('windows'), 'checksums must wait for verified Windows MSI')
  end
end
puts "PASS: #{checks} Windows workflow checks"
