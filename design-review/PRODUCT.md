# Tenshu

<!-- impeccable:product-schema 1 -->

## Platform

adaptive

Native desktop, Rust/Iced, targeting macOS, Windows and Linux. The schema has no desktop value; adaptive here records desktop platform variation, not a mobile product. Use desktop keyboard, windowing and accessibility expectations; do not invent iOS/Android navigation.

## Users

Everyday Bitcoin users are the primary navigation audience for this redesign. Experienced users retain explicit expert access to coin control, PSBTs, descriptors and node maintenance. Robert confirmed this choice in the discovery interview.

## Product Purpose

Manage Bitcoin wallets and protection through encrypted containers called Cubes. Help users identify their funds, complete wallet tasks, understand backup readiness and manage signers without confusing wallet, account or network scope.

## Capabilities and Constraints

A Cube may contain a Vault, Spark and eligible Liquid wallet. Vaults support policy-based signing and time-locked recovery paths; backup and recovery-path spending are distinct jobs. Connect supplies account services, including account security, contacts, plan management and applicable recovery and signing services. Availability depends on wallet type, network, authentication and feature gates.

The implementation is Rust/Iced with shared coincube-ui components. Settings can affect the app/device, account, Cube or wallet. Stable Balance is a device preference and does not travel with the seed, as the running app explains. Preserve security boundaries and current feature eligibility. Do not invent guarantees, account capabilities or pricing.

## Brand Commitments

Retain Tenshu and COINCUBE naming, established recognizable identity and existing assets. This stage reorganizes navigation and Settings within the existing product identity; no rebrand requested. Operative screens should be readable and quiet, with orange reserved for action, selection and meaningful state.

## Evidence on Hand

Live macOS walkthrough of Testnet4 Cube/Vault, mainnet Spark/Liquid and signed-in Connect, documented in the task's ux-review/live-walkthrough.md with local screenshots. Source baseline 8495c3c0f233650b8642363c455ccd429b71bc47; runtime source revision unverified. User confirmed navigation plus detailed Settings redesign before application code changes. Light theme, imported-wallet variations and full assistive-technology behavior remain unverified. Private screenshot data must not be used in external concepts; use clearly synthetic fixtures.

## Product Principles

- Everyday tasks lead; expert tools remain explicit and reachable.
- Identity and affected scope accompany consequential settings.
- Backup readiness is distinct from recovery-path availability.
- Explain blocked actions with their actual cause and next step.
- Preserve security consequences and product truth throughout visual changes.
