#!/usr/bin/env ruby
# Structural regression checks use Ruby's standard-library YAML parser.
require 'yaml'

root = File.expand_path('../../..', __dir__)
checks = 0
check = lambda do |condition, message|
  abort message unless condition
  checks += 1
end

PINNED = /@[a-f0-9]{40}$/
SIGNING_JOBS = %w[windows_sign_executables windows_sign_msi].freeze
WINDOWS_JOBS = %w[windows_build windows_sign_executables windows_package windows_sign_msi windows_verify].freeze
EXECUTABLES = %w[coincube.exe coincube-spark-bridge.exe coincube-mcp.exe].freeze
SIGNER_USES = %w[actions/checkout@ actions/download-artifact@ azure/login@ azure/artifact-signing-action@ actions/upload-artifact@].freeze
LOGOUT = "az logout\naz account clear\nexit 0\n"

%w[releases nightly].each do |workflow|
  data = YAML.load_file(File.join(root, '.github/workflows', "#{workflow}.yml"))
  jobs = data.fetch('jobs')
  prefix = "#{workflow}: "
  step = ->(job, name) { jobs.fetch(job).fetch('steps').find { |s| s['name'] == name } or abort(prefix + "#{job} lacks #{name}") }
  order = lambda do |job, wanted, message|
    names = jobs.fetch(job).fetch('steps').map { |s| s['name'] }
    positions = wanted.map { |name| names.index(name) }
    check.call(positions.none?(&:nil?) && positions == positions.sort, prefix + message)
  end

  check.call(data.fetch('permissions').fetch('id-token', 'none') == 'none', prefix + 'global OIDC permission')
  check.call(!jobs.key?('windows'), prefix + 'combined build-and-sign job returned')
  WINDOWS_JOBS.each { |name| check.call(jobs.key?(name), prefix + "missing #{name}") }

  environment = workflow == 'releases' ? 'windows-release-signing' : 'windows-nightly-signing'
  jobs.each do |name, job|
    if SIGNING_JOBS.include?(name)
      check.call(job.fetch('permissions') == { 'contents' => 'read', 'id-token' => 'write' }, prefix + "#{name} permissions")
      check.call(job['environment'] == environment, prefix + "#{name} signing environment")
    else
      check.call(job.fetch('permissions', {}).fetch('id-token', 'none') != 'write', prefix + "OIDC leaked to #{name}")
      check.call(!job.key?('environment') || !job['environment'].to_s.include?('signing'), prefix + "signing environment leaked to #{name}")
    end
    if WINDOWS_JOBS.include?(name) && !SIGNING_JOBS.include?(name)
      check.call(job.fetch('permissions') == { 'contents' => 'read' }, prefix + "#{name} must be read-only")
    end
    check.call(!job['continue-on-error'] && job.fetch('steps', []).none? { |s| s['continue-on-error'] }, prefix + "failure bypass in #{name}")
    platforms = job.fetch('strategy', {}).fetch('matrix', {}).fetch('include', [])
    check.call(platforms.none? { |leg| leg['platform'] == 'windows' }, prefix + 'unsigned Windows matrix leg')
  end

  expected_guard = if workflow == 'releases'
    "github.repository == 'coincubetech/coincube' && startsWith(github.ref, 'refs/tags/v') && (github.event_name == 'push' || github.event_name == 'workflow_dispatch')"
  else
    "github.repository == 'coincubetech/coincube' && github.ref == 'refs/heads/master' && needs.check.outputs.should_run == 'true' && (github.event_name == 'schedule' || github.event_name == 'workflow_dispatch')"
  end
  check.call(jobs.fetch('windows_build').fetch('if') == expected_guard, prefix + 'trusted source/event guard')
  # Later jobs inherit the guard by depending on the build; their own `if` could override it.
  WINDOWS_JOBS.drop(1).each { |name| check.call(!jobs.fetch(name).key?('if'), prefix + "#{name} overrides the build guard") }
  {
    'windows_sign_executables' => 'windows_build',
    'windows_package' => %w[windows_build windows_sign_executables],
    'windows_sign_msi' => %w[windows_build windows_package],
    'windows_verify' => %w[windows_build windows_sign_msi]
  }.each { |name, needs| check.call(jobs.fetch(name).fetch('needs') == needs, prefix + "#{name} dependency chain") }

  # Credentialed jobs: pinned actions plus this repository's verifier only.
  SIGNING_JOBS.each do |name|
    steps = jobs.fetch(name).fetch('steps')
    steps.each do |s|
      if s['uses']
        check.call(SIGNER_USES.any? { |allowed| s['uses'].start_with?(allowed) } && s['uses'].match?(PINNED),
          prefix + "#{name} runs unapproved action #{s['uses']}")
      else
        script = s.fetch('run')
        check.call(script.start_with?(". ./contrib/release/windows/signing.ps1\n") || script == LOGOUT,
          prefix + "#{name} runs unapproved command in #{s['name']}")
        check.call(s['shell'] == 'pwsh', prefix + "#{name} #{s['name']} shell")
      end
    end
    order.call(name, ['Require Windows signing configuration', steps[2]['name'], steps[3]['name'], 'Azure login (OIDC)',
      steps.find { |s| s.fetch('uses', '').start_with?('azure/artifact-signing-action@') }['name'], 'Azure logout',
      steps[-2]['name'], steps[-1]['name']], "#{name} configure/download/check/login/sign/logout/verify/upload order")
    check.call(steps[0].fetch('uses', '').start_with?('actions/checkout@') && steps[1]['name'] == 'Require Windows signing configuration' &&
      steps[2].fetch('uses', '').start_with?('actions/download-artifact@') && steps[-1].fetch('uses', '').start_with?('actions/upload-artifact@'),
      prefix + "#{name} must only sign downloaded files and upload them last")
    check.call(steps.count { |s| s.fetch('uses', '').start_with?('azure/artifact-signing-action@') } == 1, prefix + "#{name} signer count")
    check.call(step.call(name, 'Azure logout')['if'] == 'always()', prefix + "#{name} logout must always run")
    required = step.call(name, 'Require Windows signing configuration').fetch('env')
    %w[AZURE_TENANT_ID AZURE_CLIENT_ID AZURE_SUBSCRIPTION_ID AZURE_ENDPOINT AZURE_CODE_SIGNING_NAME AZURE_CERT_PROFILE_NAME].each do |var|
      check.call(required[var] == "${{ vars.#{var} }}", prefix + "#{name} missing configuration #{var}")
    end
    login = step.call(name, 'Azure login (OIDC)')
    check.call(!login.fetch('with').key?('creds'), prefix + "#{name} OIDC login credentials")
    inputs = steps.find { |s| s.fetch('uses', '').start_with?('azure/artifact-signing-action@') }.fetch('with')
    check.call(inputs['file-digest'] == 'SHA256' && inputs['timestamp-digest'] == 'SHA256' &&
      inputs['timestamp-rfc3161'] == 'http://timestamp.acs.microsoft.com', prefix + "#{name} digest/timestamp configuration")
    check.call(inputs['cache-dependencies'] == false && inputs['exclude-environment-credential'] == true,
      prefix + "#{name} cached signer or standing credential fallback")
    check.call(inputs.keys.none? { |key| key.start_with?('azure-') }, prefix + "#{name} standing Azure credentials")
  end

  exe_signer = jobs.fetch('windows_sign_executables').fetch('steps').find { |s| s.fetch('uses', '').start_with?('azure/artifact-signing-action@') }
  expected_paths = EXECUTABLES.map { |exe| "${{ github.workspace }}\\windows-signing\\#{exe}" }
  check.call(exe_signer.fetch('with').fetch('files').lines.map(&:strip) == expected_paths, prefix + 'all three exact payload paths')
  check.call(step.call('windows_sign_executables', 'Require exact unsigned payload')['run'].include?(
    "Assert-WindowsStagedFiles -Root windows-signing -Names @('coincube.exe', 'coincube-spark-bridge.exe', 'coincube-mcp.exe')"),
    prefix + 'exact unsigned payload before login')
  verify = step.call('windows_sign_executables', 'Verify executable signatures')['run']
  EXECUTABLES.each { |exe| check.call(verify.include?("windows-signing/#{exe}"), prefix + "unverified signed #{exe}") }
  msi_name = 'coincube-desktop-$env:COINCUBE_VERSION-$env:WINDOWS_TARGET.msi'
  check.call(step.call('windows_sign_msi', 'Require exact unsigned installer')['run'].include?(%(-Root windows-signing -Names @("#{msi_name}"))),
    prefix + 'exact unsigned installer before login')
  check.call(step.call('windows_sign_msi', 'Verify MSI signature')['run'].include?("windows-signing/#{msi_name}"), prefix + 'unverified MSI')

  # Artifact handoffs: each job consumes what the previous one produced.
  uploads = ->(job) { jobs.fetch(job).fetch('steps').select { |s| s.fetch('uses', '').start_with?('actions/upload-artifact@') } }
  downloads = ->(job) { jobs.fetch(job).fetch('steps').select { |s| s.fetch('uses', '').start_with?('actions/download-artifact@') } }
  WINDOWS_JOBS.each do |name|
    (uploads.call(name) + downloads.call(name)).each { |s| check.call(s['uses'].match?(PINNED), prefix + "#{name} unpinned artifact action") }
  end
  handoffs = {
    'windows_sign_executables' => [%w[windows-unsigned-executables windows-signing]],
    'windows_package' => [%w[windows-signed-executables target/minimal], %w[windows-installer-icon contrib/release/wix]],
    'windows_sign_msi' => [%w[windows-unsigned-msi windows-signing]],
    'windows_verify' => [%w[windows-signed-msi target/wix]]
  }
  handoffs.each do |name, wanted|
    got = downloads.call(name).map { |s| [s.fetch('with')['name'], s.fetch('with')['path']] }
    check.call(got == wanted, prefix + "#{name} downloads")
  end
  intermediates = WINDOWS_JOBS[0..3].flat_map { |name| uploads.call(name) }
  check.call(intermediates.map { |s| s.fetch('with')['name'] }.sort ==
    %w[windows-installer-icon windows-signed-executables windows-signed-msi windows-unsigned-executables windows-unsigned-msi],
    prefix + 'intermediate artifact names')
  intermediates.each do |s|
    check.call(s.fetch('with')['retention-days'] == 1, prefix + "#{s['name']} must expire quickly")
    check.call(!s.fetch('with')['name'].start_with?('release-', 'coincube-desktop-'), prefix + "#{s['name']} could be published")
  end
  check.call(uploads.call('windows_verify').length == 1, prefix + 'verification stages exactly one artifact')

  package = 'windows_package'
  order.call(package, ['Download signed Windows executables', 'Verify executable signatures before packaging', 'Package Windows MSI',
    'Upload unsigned Windows MSI'], 'package verifies signed payload before packaging')
  pre_package = step.call(package, 'Verify executable signatures before packaging')['run']
  EXECUTABLES.each { |exe| check.call(pre_package.include?("target/minimal/#{exe}"), prefix + "unverified #{exe} before packaging") }
  check.call(pre_package.index('Assert-WindowsStagedFiles') < pre_package.index('Assert-WindowsSignature'), prefix + 'package exact payload')

  final = workflow == 'releases' ? 'Stage Windows MSI' : 'Upload Windows artifact'
  order.call('windows_verify', ['Download signed Windows MSI', 'Verify MSI signature before extraction', 'Install Rust',
    'Verify Spark bridge is in the MSI', 'Re-verify MSI before staging', final], 'verify/extract/test/re-verify/stage order')
  check.call(jobs.fetch('windows_verify').fetch('steps').last['name'] == final, prefix + 'nothing runs after staging')
  extracted = step.call('windows_verify', 'Verify Spark bridge is in the MSI')['run']
  check.call(extracted.index('Assert-WindowsMsiPayload') < extracted.index('cargo test'), prefix + 'execute before verifying extracted payload')
  first_verify = step.call('windows_verify', 'Verify MSI signature before extraction')['run']
  check.call(first_verify.include?("Assert-WindowsStagedFiles -Root target/wix") && first_verify.include?("Assert-WindowsSignature -Paths \"target/wix/#{msi_name}\""),
    prefix + 'verify downloaded MSI before extraction')
  check.call(step.call('windows_verify', 'Re-verify MSI before staging')['run'].include?("Assert-WindowsSignature -Paths \"target/wix/#{msi_name}\""),
    prefix + 're-verify staged MSI')
  staged = step.call('windows_verify', final).fetch('with')
  check.call(staged['path'] == 'target/wix/coincube-desktop-${{ env.COINCUBE_VERSION }}-${{ env.WINDOWS_TARGET }}.msi', prefix + 'staged MSI path')
  expected_name = workflow == 'releases' ? 'release-${{ env.WINDOWS_TARGET }}' : 'coincube-desktop-nightly-${{ env.BUILD_TAG }}-${{ env.WINDOWS_TARGET }}'
  check.call(staged['name'] == expected_name, prefix + 'staged artifact name')

  if workflow == 'releases'
    check.call(jobs.fetch('checksums').fetch('needs') == %w[release windows_verify], 'checksums must wait for verified Windows MSI')
  end
end
puts "PASS: #{checks} Windows workflow checks"
