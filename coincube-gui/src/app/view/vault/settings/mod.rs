#[cfg(test)]
mod capture;
pub mod general;

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

use iced::widget::{pick_list, Column};
use iced::{
    alignment,
    widget::{radio, scrollable, tooltip as iced_tooltip, Space},
    Alignment, Length,
};

use coincube_core::{
    descriptors::CoincubePolicy,
    miniscript::bitcoin::{bip32::Fingerprint, Network},
    wallet_descriptor::WalletDescriptor,
};
use coincubed::config::BitcoindRpcAuth;

use super::super::{dashboard, message::*};

use coincube_ui::{
    component::{badge, button, card, date_picker, form, separation, text::*},
    icon,
    theme::{self},
    widget::{Button, ColumnExt, Container, Element, ProgressBar, Row, TextInput},
};

use crate::{
    app::{
        cache::Cache,
        menu::{CubeSettingsOption, CubeSubMenu, Menu, VaultSubMenu},
        settings::ProviderKey,
        view::vault::hw,
    },
    help,
    hw::HardwareWallet,
    node::{
        bitcoind::{NodeFlavor, RpcAuthType, RpcAuthValues},
        electrum::{self, validate_domain_checkbox},
    },
};

fn header<'a>(title: &'a str, _msg: SettingsMessage) -> Row<'a, Message> {
    // Matches the plain `h3` heading style used on the Home / Wallets
    // page so Vault Settings sub-pages don't pick up extra button
    // padding. The tertiary rail already surfaces the active sub-page,
    // so no breadcrumb is rendered here.
    Row::new().push(h3(title).bold())
}

pub fn list<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    _is_remote_backend: bool,
) -> Element<'a, Message> {
    // Defensive fallback for Settings(None) + no inner section state.
    let header = Button::new(text("Settings").size(30).bold())
        .style(theme::button::transparent)
        .on_press(Message::Menu(Menu::Vault(VaultSubMenu::Settings(Some(
            crate::app::menu::SettingsOption::Wallet,
        )))));

    dashboard(
        menu,
        cache,
        Column::new()
            .spacing(20)
            .width(Length::Fill)
            .push(header)
            .push(p1_regular(
                "Open Wallet & signers to review this wallet's settings.",
            )),
    )
}

pub fn link<'a>(url: &'a str, link_text: &'static str) -> Element<'a, Message> {
    iced_tooltip::Tooltip::new(
        button::link(Some(icon::link_icon()), link_text)
            .on_press(Message::OpenUrl(url.to_string())),
        Container::new(text(url))
            .style(theme::card::simple)
            .padding(10),
        iced_tooltip::Position::Bottom,
    )
    .into()
}

pub fn bitcoind_settings<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    settings: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    // Each panel is a titled group that carries its own 32px top margin.
    dashboard(
        menu,
        cache,
        Column::with_children(settings).width(Length::Fill),
    )
}

pub fn import_export<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    supports_inheritance: bool,
) -> Element<'a, Message> {
    use crate::app::view::settings::page;
    let settings = |m: SettingsMessage| Some(Message::Settings(m));

    // Recovery Kit belongs to Cube → Backup & protection. Navigate to its
    // rendered owner; never start its wizard from this Vault panel.
    let mut protection: Vec<Element<'a, Message>> = vec![page::row("Recovery Kit")
        .description("Part of Cube backup.")
        .control(page::button(
            "tenshu.settings.vault-data.backup",
            "Open Backup & protection",
            Some(Message::Menu(Menu::Cube(CubeSubMenu::Settings(
                CubeSettingsOption::Recovery,
            )))),
        ))
        .into()];
    if supports_inheritance {
        protection.push(
            page::row("Encrypted descriptor")
                .description("Needed with the keys to restore this Vault. Decrypts with one of your signing devices or xpubs.")
                .control(page::button(
                    "tenshu.settings.vault-data.encrypted",
                    "Export encrypted…",
                    settings(SettingsMessage::ExportEncryptedDescriptor),
                ))
                .into(),
        );
    }

    let mut content = Column::new().width(Length::Fill).push(page::group(
        "Protection material",
        Some("Anyone holding this can see every address and balance in the Vault.".into()),
        page::rows(protection),
    ));

    let mut portability: Vec<Element<'a, Message>> = Vec::new();
    if supports_inheritance {
        portability.push(
            page::row("Wallet file")
                .description("Unencrypted. Moves wallet information to another device; imports are checked against this network first.")
                .control(page::button(
                    "tenshu.settings.vault-data.export-wallet",
                    "Export…",
                    settings(SettingsMessage::ExportWallet),
                ))
                .control(page::button(
                    "tenshu.settings.vault-data.import-wallet",
                    "Import…",
                    settings(SettingsMessage::ImportWallet),
                ))
                .into(),
        );
    }
    let mut formats: Vec<Element<'a, Message>> = vec![
        page::row("Labels")
            .description("BIP 329 format other wallets can read.")
            .control(page::button(
                "tenshu.settings.vault-data.labels",
                "Export…",
                settings(SettingsMessage::ExportLabels),
            ))
            .into(),
        page::row("Transactions")
            .description("CSV of past transactions. Fiat uses this Cube's rate source.")
            .control(page::button(
                "tenshu.settings.vault-data.transactions",
                "Export…",
                settings(SettingsMessage::ExportTransactions),
            ))
            .into(),
    ];
    if supports_inheritance {
        formats.push(
            page::row("Plaintext descriptor")
                .description("For hardware registration or other wallets. Reveals the wallet policy and public keys.")
                .control(page::button(
                    "tenshu.settings.vault-data.plaintext",
                    "Export…",
                    settings(SettingsMessage::ReviewPlaintextDescriptorExport),
                ))
                .into(),
        );
    }
    let mut records = Column::new().width(Length::Fill);
    if !portability.is_empty() {
        records = records.push(page::rows(portability)).push(
            Container::new(
                Row::new()
                    .spacing(12)
                    .align_y(Alignment::Center)
                    .push(Container::new(page::rule()).width(Length::Fill))
                    .push(coincube_ui::component::tenshu::caption("Other formats"))
                    .push(Container::new(page::rule()).width(Length::Fill)),
            )
            .padding(iced::Padding::default().top(24.0).bottom(12.0)),
        );
    }
    records = records.push(page::rows(formats));
    content = content.push(page::group(
        "Portability and records",
        Some("Plaintext files reveal the Vault's full history. They hold no private keys, and records do not replace recovery material.".into()),
        records,
    ));

    dashboard(menu, cache, content)
}

/// Vault → Settings → Connections: the node backend's health with Node
/// settings one click away, and one row for the Cube's Connect session that
/// opens Cube › Connections.
pub fn connections<'a>(cache: &'a Cache) -> Element<'a, Message> {
    use crate::app::view::settings::page;
    let progress = cache.sync_progress();
    let block = coincube_ui::component::amount::group_digits(&cache.blockheight().to_string(), ",");
    let node = if progress >= 1.0 {
        page::row("Node backend and sync")
            .accessory(page::status(page::Status::Ok, "Synced"))
            .description(format!("Synced to block {block}."))
    } else {
        page::row("Node backend and sync")
            .accessory(page::status(
                page::Status::Busy,
                format!("Syncing {:.1}%", crate::utils::syncing_percent(progress)),
            ))
            .description(format!("At block {block}. Balances may be out of date."))
    }
    .control(page::button(
        "tenshu.settings.vault-connections.node",
        "Node settings",
        Some(Message::Menu(Menu::Vault(VaultSubMenu::Settings(Some(
            crate::app::menu::SettingsOption::Node,
        ))))),
    ));
    // The App wraps this in the dashboard (`connect_settings_content`).
    page::group(
        "Health",
        None,
        page::rows([
            node.into(),
            crate::app::view::settings::connections::cube_connections_row(
                cache,
                "tenshu.settings.vault-connections.cube",
            ),
        ]),
    )
}

pub fn plaintext_export_review<'a>() -> Element<'a, Message> {
    card::modal(
        Column::new()
            .spacing(15)
            .push(h4_bold("Export a plaintext descriptor?"))
            .push(text(
                "This file is not encrypted. It reveals your wallet policy and public keys. Choose a private location in the file picker and share it only with an app you trust.",
            ))
            .push(
                Row::new()
                    .spacing(10)
                    .push(
                        button::secondary(None, "Cancel").on_press(Message::Settings(
                            SettingsMessage::CancelPlaintextDescriptorExport,
                        )),
                    )
                    .push(
                        button::primary(None, "Choose export location…").on_press(
                            Message::Settings(SettingsMessage::ExportPlaintextDescriptor),
                        ),
                    ),
            ),
    )
    .into()
}

pub fn remote_backend_section<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    email_form: &form::Value<String>,
    processing: bool,
    success: bool,
) -> Element<'a, Message> {
    let header = header("Backend", SettingsMessage::EditRemoteBackendSettings);

    let content = card::simple(
        Column::new()
            .spacing(20)
            .push(text("Grant access to wallet to another user"))
            .push(
                form::Form::new_trimmed("User email", email_form, |email| {
                    Message::Settings(SettingsMessage::RemoteBackendSettings(
                        RemoteBackendSettingsMessage::EditInvitationEmail(email),
                    ))
                })
                .warning("Email is invalid")
                .size(P1_SIZE)
                .padding(10),
            )
            .push(
                Row::new()
                    .push(if success {
                        Some(text("Invitation was sent").style(theme::text::success))
                    } else {
                        None
                    })
                    .push(Space::new().width(Length::Fill))
                    .push(button::secondary(None, "Send invitation").on_press_maybe(
                        if !processing && email_form.valid {
                            Some(Message::Settings(SettingsMessage::RemoteBackendSettings(
                                RemoteBackendSettingsMessage::SendInvitation,
                            )))
                        } else {
                            None
                        },
                    )),
            ),
    )
    .width(Length::Fill);

    dashboard(
        menu,
        cache,
        Column::new()
            .spacing(20)
            .push(header)
            .push(content)
            .push(link(
                help::CHANGE_BACKEND_OR_NODE_URL,
                "I want to connect to my own node",
            )),
    )
}

pub fn bitcoind_edit<'a>(
    is_configured_node_type: bool,
    network: Network,
    blockheight: i32,
    addr: &form::Value<String>,
    rpc_auth_vals: &RpcAuthValues,
    selected_auth_type: &RpcAuthType,
    processing: bool,
) -> Element<'a, SettingsEditMessage> {
    let mut col = Column::new().spacing(20);
    if is_configured_node_type && blockheight != 0 {
        col = col
            .push(
                Row::new()
                    .push(
                        Row::new()
                            .push(badge::badge(icon::network_icon()))
                            .push(
                                Column::new()
                                    .push(text("Network:"))
                                    .push(text(network.to_string()).bold()),
                            )
                            .spacing(10)
                            .width(Length::FillPortion(1)),
                    )
                    .push(
                        Row::new()
                            .push(badge::badge(icon::block_icon()))
                            .push(
                                Column::new()
                                    .push(text("Block Height:"))
                                    .push(text(blockheight.to_string()).bold()),
                            )
                            .spacing(10)
                            .width(Length::FillPortion(1)),
                    ),
            )
            .push(separation().width(Length::Fill));
    }

    col = col
        .push(
            [RpcAuthType::CookieFile, RpcAuthType::UserPass]
                .iter()
                .fold(
                    Row::new()
                        .push(text("RPC authentication:").small().bold())
                        .spacing(10),
                    |row, auth_type| {
                        row.push(radio(
                            format!("{}", auth_type),
                            *auth_type,
                            Some(*selected_auth_type),
                            SettingsEditMessage::BitcoindRpcAuthTypeSelected,
                        ))
                        .spacing(30)
                        .align_y(Alignment::Center)
                    },
                ),
        )
        .push(match selected_auth_type {
            RpcAuthType::CookieFile => Column::new()
                .push(
                    form::Form::new_trimmed(
                        "Cookie file path",
                        &rpc_auth_vals.cookie_path,
                        |value| SettingsEditMessage::FieldEdited("cookie_file_path", value),
                    )
                    .warning("Please enter a valid filesystem path")
                    .size(P1_SIZE)
                    .padding(5),
                )
                .spacing(5),
            RpcAuthType::UserPass => Column::new()
                .push(
                    Row::new()
                        .push(
                            form::Form::new_trimmed("User", &rpc_auth_vals.user, |value| {
                                SettingsEditMessage::FieldEdited("user", value)
                            })
                            .warning("Please enter a valid user")
                            .size(P1_SIZE)
                            .padding(5),
                        )
                        .push(
                            form::Form::new_trimmed("Password", &rpc_auth_vals.password, |value| {
                                SettingsEditMessage::FieldEdited("password", value)
                            })
                            .warning("Please enter a valid password")
                            .size(P1_SIZE)
                            .padding(5),
                        )
                        .spacing(10),
                )
                .spacing(5),
        })
        .push(
            Column::new()
                .push(text("Socket address:").bold().small())
                .push(
                    form::Form::new_trimmed("Socket address:", addr, |value| {
                        SettingsEditMessage::FieldEdited("socket_address", value)
                    })
                    .warning("Please enter a valid address")
                    .size(P1_SIZE)
                    .padding(5),
                )
                .spacing(5),
        );

    let mut cancel_button = button::transparent(None, " Cancel ").padding(5);
    let mut confirm_button = button::secondary(None, " Save ").padding(5);
    if !processing {
        cancel_button = cancel_button.on_press(SettingsEditMessage::Cancel);
        confirm_button = confirm_button.on_press(SettingsEditMessage::Confirm);
    }

    card::simple(Container::new(
        Column::new()
            .push(
                Row::new()
                    .push(badge::badge(icon::bitcoin_icon()))
                    .push(text("Bitcoin Core").bold())
                    .padding(10)
                    .spacing(20)
                    .align_y(Alignment::Center)
                    .width(Length::Fill),
            )
            .push(separation().width(Length::Fill))
            .push(col)
            .push(
                Container::new(
                    Row::new()
                        .push(cancel_button)
                        .push(confirm_button)
                        .spacing(10)
                        .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .align_x(alignment::Horizontal::Right),
            )
            .spacing(20),
    ))
    .width(Length::Fill)
    .into()
}

#[allow(clippy::too_many_arguments)]
pub fn bitcoind<'a>(
    is_configured_node_type: bool,
    network: Network,
    config: &coincubed::config::BitcoindConfig,
    blockheight: i32,
    is_running: Option<bool>,
    can_edit: bool,
    // `Some(flavour)` for the internal managed node (renders a Core/Knots
    // switcher); `None` for an external node (flavour unknown/not switchable).
    managed_flavor: Option<NodeFlavor>,
    // `getnetworkinfo.subversion` of the node actually serving this backend,
    // polled alongside its network stats. `None` before the first poll, or when
    // the backend isn't a local node.
    subversion: Option<&'a str>,
) -> Element<'a, SettingsEditMessage> {
    let mut col = Column::new().spacing(20);
    if is_configured_node_type && blockheight != 0 {
        col = col
            .push(
                Row::new()
                    .push(
                        Row::new()
                            .push(badge::badge(icon::network_icon()))
                            .push(
                                Column::new()
                                    .push(text("Network:"))
                                    .push(text(network.to_string()).bold()),
                            )
                            .spacing(10)
                            .width(Length::FillPortion(1)),
                    )
                    .push(
                        Row::new()
                            .push(badge::badge(icon::block_icon()))
                            .push(
                                Column::new()
                                    .push(text("Block Height:"))
                                    .push(text(blockheight.to_string()).bold()),
                            )
                            .spacing(10)
                            .width(Length::FillPortion(1)),
                    ),
            )
            .push(separation().width(Length::Fill));
    }

    let mut rows = vec![];
    if is_configured_node_type {
        match &config.rpc_auth {
            BitcoindRpcAuth::CookieFile(path) => {
                rows.push(("Cookie file path:", path.to_str().unwrap().to_string()));
            }
            BitcoindRpcAuth::UserPass(user, password) => {
                rows.push(("User:", user.clone()));
                rows.push(("Password:", password.clone()));
            }
        }
        rows.push(("Socket address:", config.addr.to_string()));
    }

    let mut col_fields = Column::new();
    for (k, v) in rows {
        col_fields = col_fields.push({
            let t = if k == "Password:" {
                "*".to_string().repeat(v.len())
            } else {
                v.clone()
            };
            Row::new()
                .push(Container::new(text(k).bold().small()).width(Length::FillPortion(1)))
                .push(
                    Container::new(
                        scrollable(
                            Column::new()
                                .push(Space::new().height(Length::Fixed(10.0)))
                                .push(text(t).small())
                                // Space between the text and the scrollbar
                                .push(Space::new().height(Length::Fixed(10.0))),
                        )
                        .direction(scrollable::Direction::Horizontal(
                            scrollable::Scrollbar::new().width(2).scroller_width(2),
                        )),
                    )
                    .align_x(alignment::Horizontal::Right)
                    .padding(10)
                    .width(Length::FillPortion(3)),
                )
                .push(Space::new().width(10))
                .push(
                    Button::new(icon::clipboard_icon())
                        .style(theme::button::transparent_border)
                        .on_press(SettingsEditMessage::Clipboard(v.to_string())),
                )
                .align_y(Alignment::Center)
        });
    }

    // Managed node → a Core/Knots dropdown (independent of `can_edit`, which
    // governs the external-node RPC form and is false for the managed node);
    // external node → a static label (its flavour isn't known to us and can't be
    // switched from here).
    let flavor_header: Element<'a, SettingsEditMessage> = match managed_flavor {
        Some(flavor) => pick_list(
            vec![NodeFlavor::Knots, NodeFlavor::Core],
            Some(flavor),
            SettingsEditMessage::SwitchManagedFlavor,
        )
        .style(theme::pick_list::primary)
        .padding(8)
        .into(),
        // An external node's flavour isn't ours to switch, but the version below
        // still names it, so this stays a generic label rather than a guess.
        None => text("Bitcoin node").bold().into(),
    };
    // What is actually running, straight from the node. Worth showing next to the
    // switcher precisely because the two can disagree — a datadir configured for
    // one flavour falls back to the other flavour's binary when the configured one
    // isn't installed.
    let version_label = subversion.and_then(crate::node::bitcoind::node_version_label);

    card::simple(Container::new(
        Column::new()
            .push(
                Row::new()
                    .push(
                        Row::new()
                            .push(badge::badge(icon::bitcoin_icon()))
                            .push(flavor_header)
                            .push(
                                version_label
                                    .map(|v| text(v).small().style(theme::text::secondary)),
                            )
                            .push(if is_configured_node_type {
                                Some(is_running_label(is_running))
                            } else {
                                None
                            })
                            .spacing(20)
                            .align_y(Alignment::Center)
                            .width(Length::Fill),
                    )
                    .push(if can_edit {
                        Button::new(icon::pencil_icon())
                            .style(theme::button::transparent_border)
                            .on_press(SettingsEditMessage::Select)
                    } else {
                        Button::new(icon::pencil_icon()).style(theme::button::transparent_border)
                    })
                    .align_y(Alignment::Center),
            )
            .push(separation().width(Length::Fill))
            .push(col.push(col_fields))
            .spacing(20),
    ))
    .width(Length::Fill)
    .into()
}

/// Confirmation modal shown before a managed-node flavour switch (Core ↔ Knots).
/// Switching restarts the shared node and may download the other client, so we
/// gate it behind an explicit confirm. Rendered as a modal body by
/// `BitcoindSettingsState::view`.
///
/// The second paragraph is direction-specific because the two swaps have
/// genuinely different consequences for the chain the node follows — see
/// [`flavor_switch_chain_note`].
pub fn flavor_switch_confirm<'a>(target: NodeFlavor) -> Element<'a, NodeSettingsMessage> {
    card::modal(
        Column::new()
            .spacing(20)
            .push(
                text(format!(
                    "Switch node software to {}?",
                    target.display_name()
                ))
                .bold()
                .size(20),
            )
            .push(
                text(
                    "This restarts your managed node for every Vault (and downloads the \
                     client if it isn't already installed, ~50 MB). Your Vaults keep \
                     working; the node reconnects on the same port once it's back.",
                )
                .size(14)
                .style(theme::text::secondary),
            )
            .push(
                text(flavor_switch_chain_note(target))
                    .size(14)
                    .style(theme::text::secondary),
            )
            .push(
                Row::new()
                    .spacing(10)
                    .push(
                        button::secondary(None, "Cancel")
                            .width(Length::FillPortion(1))
                            .on_press(NodeSettingsMessage::CancelFlavorSwitch),
                    )
                    .push(
                        button::primary(None, "Switch")
                            .width(Length::FillPortion(1))
                            .on_press(NodeSettingsMessage::ConfirmFlavorSwitch),
                    ),
            ),
    )
    .width(Length::Fixed(500.0))
    .into()
}

/// Confirmation shown when the user asks to switch to the local node while
/// the Vault is still scanning. The switch stops the Connect backend, and with
/// it the scan, so the user decides whether that is worth it.
pub fn switch_discards_scan_confirm<'a>(
    scan: crate::app::local_switch::RunningScan,
) -> Element<'a, NodeSettingsMessage> {
    use crate::app::local_switch::RunningScan;
    let (title, body) = match scan {
        RunningScan::WalletSync => (
            "Your Vault is still scanning",
            "COINCUBE | Connect is still looking for this Vault's transactions. \
             Switching to your local node now stops that scan and discards what it has \
             found so far; the local node then has to scan again from the start, which \
             can take a long time.",
        ),
        RunningScan::Rescan => (
            "A rescan is still running",
            "This Vault is still rescanning the blockchain. Switching to your local node \
             now stops the rescan and discards its progress; it will have to start again \
             on the local node.",
        ),
    };
    card::modal(
        Column::new()
            .spacing(20)
            .push(text(title).bold().size(20))
            .push(text(body).size(14).style(theme::text::secondary))
            .push(
                text("If you wait, Tenshu can switch for you once the scan has finished.")
                    .size(14)
                    .style(theme::text::secondary),
            )
            .push(
                Row::new()
                    .spacing(10)
                    .push(
                        button::secondary(None, "Keep scanning")
                            .width(Length::FillPortion(1))
                            .on_press(NodeSettingsMessage::CancelSwitchDiscardingScan),
                    )
                    .push(
                        button::primary(None, "Switch anyway")
                            .width(Length::FillPortion(1))
                            .on_press(NodeSettingsMessage::ConfirmSwitchDiscardingScan),
                    ),
            ),
    )
    .width(Length::Fixed(500.0))
    .into()
}

/// The local node requested at Vault creation, while the App installs it in
/// the background or after that install failed. `None` when there is nothing
/// to show.
pub fn background_node_install<'a>(
    status: &crate::app::cache::BackgroundNodeInstall,
) -> Option<Element<'a, NodeSettingsMessage>> {
    use crate::app::cache::BackgroundNodeInstall;
    let body: Element<'a, NodeSettingsMessage> = match status {
        BackgroundNodeInstall::Idle => return None,
        BackgroundNodeInstall::Downloading(percent) => {
            let mut col = Column::new().spacing(10).push(
                p2_regular(match percent {
                    Some(percent) => format!("Downloading the local node… {percent:.0}%"),
                    None => "Downloading the local node…".to_string(),
                })
                .style(theme::text::secondary),
            );
            if let Some(percent) = percent {
                col = col.push(ProgressBar::new(0.0..=100.0, *percent).length(Length::Fill));
            }
            col.into()
        }
        BackgroundNodeInstall::Starting => {
            p2_regular("Verifying, installing and starting the local node…")
                .style(theme::text::secondary)
                .into()
        }
        BackgroundNodeInstall::Failed(error) => Column::new()
            .spacing(10)
            .push(p2_regular(format!("Setup paused: {error}")).style(theme::text::warning))
            .push(
                button::secondary(None, "Retry")
                    .padding([8, 15])
                    .on_press(NodeSettingsMessage::RetryBackgroundNodeSetup),
            )
            .into(),
    };
    Some(
        Container::new(
            Column::new()
                .spacing(8)
                .push(caption("Local node setup"))
                .push(
                    p2_regular(
                        "Your Vault uses COINCUBE | Connect until the local node is \
                         installed and synced.",
                    )
                    .style(theme::text::secondary),
                )
                .push(body),
        )
        .padding(15)
        .width(Length::Fill)
        .style(theme::card::border)
        .into(),
    )
}

/// Standing notice on the Node settings when the pending local node is pruned
/// past this Vault's history: neither the manual nor the automatic switch will
/// happen, and this says why.
pub fn pruned_node_notice<'a, M: 'a>(
    why: crate::app::local_switch::PrunedHistory,
) -> Element<'a, M> {
    Container::new(
        Column::new()
            .spacing(8)
            .push(caption("Local wallet history unavailable"))
            .push(p2_regular(why.to_string()).style(theme::text::warning)),
    )
    .padding(15)
    .width(Length::Fill)
    .style(theme::card::border)
    .into()
}

/// What switching *to* `target` actually changes.
///
/// Policy only. Neither build we ship enforces consensus rules the other does
/// not, so a swap no longer implies the node can end up on a different chain —
/// which is what the previous, deliberately asymmetric version of this copy had
/// to warn about. The reasoning behind that asymmetry is kept, past-tense, in
/// `node::revalidate`'s module docs, alongside the repair machinery it justified.
fn flavor_switch_chain_note(target: NodeFlavor) -> &'static str {
    match target {
        NodeFlavor::Core => {
            "Core is the reference implementation and relays with its default policy. \
             Your chain, balances, and transaction history are unaffected."
        }
        NodeFlavor::Knots => {
            "Knots relays with stricter defaults — it limits oversized data-carrier \
             (\"spam\") transactions. Your chain, balances, and transaction history are \
             unaffected."
        }
        // Never offered on this screen: the Bitcoin Blake2b provider serves its
        // own chain from its own node directory (`NodeChainFamily`), so a
        // Bitcoin Vault's node can't be switched to it.
        NodeFlavor::KnotsBlake2b => {
            "Bitcoin Knots (Bitcoin Blake2b) runs the Bitcoin Blake2b chain only and \
             can't serve this Vault's node."
        }
    }
}

pub fn electrum_edit<'a>(
    is_configured_node_type: bool,
    network: Network,
    blockheight: i32,
    addr: &form::Value<String>,
    processing: bool,
    validate_domain: bool,
) -> Element<'a, SettingsEditMessage> {
    let mut col = Column::new().spacing(20);
    if is_configured_node_type && blockheight != 0 {
        col = col
            .push(
                Row::new()
                    .push(
                        Row::new()
                            .push(badge::badge(icon::network_icon()))
                            .push(
                                Column::new()
                                    .push(text("Network:"))
                                    .push(text(network.to_string()).bold()),
                            )
                            .spacing(10)
                            .width(Length::FillPortion(1)),
                    )
                    .push(
                        Row::new()
                            .push(badge::badge(icon::block_icon()))
                            .push(
                                Column::new()
                                    .push(text("Block Height:"))
                                    .push(text(blockheight.to_string()).bold()),
                            )
                            .spacing(10)
                            .width(Length::FillPortion(1)),
                    ),
            )
            .push(separation().width(Length::Fill));
    }

    let checkbox = validate_domain_checkbox(addr, validate_domain, |b| {
        SettingsEditMessage::ValidateDomainEdited(b)
    });
    col = col.push(
        Column::new()
            .push(text("Address:").bold().small())
            .push(
                form::Form::new_trimmed("127:0.0.1:50001", addr, |value| {
                    SettingsEditMessage::FieldEdited("address", value)
                })
                .warning("Please enter a valid address")
                .size(P1_SIZE)
                .padding(5),
            )
            .push(checkbox)
            .push(text(electrum::ADDRESS_NOTES).size(P2_SIZE))
            .spacing(5),
    );

    let mut cancel_button = button::transparent(None, " Cancel ").padding(5);
    let mut confirm_button = button::secondary(None, " Save ").padding(5);
    if !processing {
        cancel_button = cancel_button.on_press(SettingsEditMessage::Cancel);
        confirm_button = confirm_button.on_press(SettingsEditMessage::Confirm);
    }

    card::simple(Container::new(
        Column::new()
            .push(
                Row::new()
                    .push(badge::badge(icon::bitcoin_icon()))
                    .push(text("Electrum").bold())
                    .padding(10)
                    .spacing(20)
                    .align_y(Alignment::Center)
                    .width(Length::Fill),
            )
            .push(separation().width(Length::Fill))
            .push(col)
            .push(
                Container::new(
                    Row::new()
                        .push(cancel_button)
                        .push(confirm_button)
                        .spacing(10)
                        .align_y(Alignment::Center),
                )
                .width(Length::Fill)
                .align_x(alignment::Horizontal::Right),
            )
            .spacing(20),
    ))
    .width(Length::Fill)
    .into()
}

pub fn electrum<'a>(
    is_configured_node_type: bool,
    network: Network,
    config: &coincubed::config::ElectrumConfig,
    blockheight: i32,
    is_running: Option<bool>,
    can_edit: bool,
) -> Element<'a, SettingsEditMessage> {
    let mut col = Column::new().spacing(20);
    if is_configured_node_type && blockheight != 0 {
        col = col
            .push(
                Row::new()
                    .push(
                        Row::new()
                            .push(badge::badge(icon::network_icon()))
                            .push(
                                Column::new()
                                    .push(text("Network:"))
                                    .push(text(network.to_string()).bold()),
                            )
                            .spacing(10)
                            .width(Length::FillPortion(1)),
                    )
                    .push(
                        Row::new()
                            .push(badge::badge(icon::block_icon()))
                            .push(
                                Column::new()
                                    .push(text("Block Height:"))
                                    .push(text(blockheight.to_string()).bold()),
                            )
                            .spacing(10)
                            .width(Length::FillPortion(1)),
                    ),
            )
            .push(separation().width(Length::Fill));
    }

    let rows = if is_configured_node_type {
        vec![("Address:", config.addr.to_string())]
    } else {
        vec![]
    };

    let mut col_fields = Column::new();
    for (k, v) in rows {
        col_fields = col_fields.push(
            Row::new()
                .push(Container::new(text(k).bold().small()).width(Length::Fill))
                .push(text(v.clone()).small())
                .push(Space::new().width(10))
                .push(
                    Button::new(icon::clipboard_icon())
                        .style(theme::button::transparent_border)
                        .on_press(SettingsEditMessage::Clipboard(v.to_string())),
                )
                .align_y(Alignment::Center),
        );
    }

    card::simple(Container::new(
        Column::new()
            .push(
                Row::new()
                    .push(
                        Row::new()
                            .push(badge::badge(icon::bitcoin_icon()))
                            .push(text("Electrum").bold())
                            .push(if is_configured_node_type {
                                Some(is_running_label(is_running))
                            } else {
                                None
                            })
                            .spacing(20)
                            .align_y(Alignment::Center)
                            .width(Length::Fill),
                    )
                    .push(if can_edit {
                        Button::new(icon::pencil_icon())
                            .style(theme::button::transparent_border)
                            .on_press(SettingsEditMessage::Select)
                    } else {
                        Button::new(icon::pencil_icon()).style(theme::button::transparent_border)
                    })
                    .align_y(Alignment::Center),
            )
            .push(separation().width(Length::Fill))
            .push(col.push(col_fields))
            .spacing(20),
    ))
    .width(Length::Fill)
    .into()
}

pub fn is_running_label<'a, T: 'a>(is_running: Option<bool>) -> Container<'a, T> {
    if let Some(running) = is_running {
        if running {
            Container::new(
                Row::new()
                    .push(icon::dot_icon().size(5).style(theme::text::success))
                    .push(text("Running").small().style(theme::text::success))
                    .align_y(Alignment::Center),
            )
        } else {
            Container::new(
                Row::new()
                    .push(icon::dot_icon().size(5).style(theme::text::error))
                    .push(text("Not running").small().style(theme::text::error))
                    .align_y(Alignment::Center),
            )
        }
    } else {
        Container::new(Column::new())
    }
}

#[allow(clippy::too_many_arguments)]
pub fn rescan<'a>(
    year: &form::Value<String>,
    month: &form::Value<String>,
    day: &form::Value<String>,
    scan_progress: Option<f64>,
    success: bool,
    processing: bool,
    can_edit: bool,
    invalid_date: bool,
    past_possible_height: bool,
    future_date: bool,
    recovery_running: bool,
) -> Element<'a, SettingsEditMessage> {
    use crate::app::view::settings::page;
    use coincube_ui::component::tenshu::ActionKind;
    let field = |label: &'static str,
                 placeholder: &'static str,
                 value: &form::Value<String>,
                 key: &'static str| {
        Column::new()
            .spacing(4)
            .width(Length::Fixed(96.0))
            .push(page::muted(label))
            .push(
                form::Form::new_trimmed(placeholder, value, move |value| {
                    SettingsEditMessage::FieldEdited(key, value)
                })
                .size(14)
                .padding(6),
            )
    };
    let ready = can_edit
        && !invalid_date
        && !processing
        && is_ok_and(&u32::from_str(&year.value), |&v| v > 0)
        && is_ok_and(&u32::from_str(&month.value), |&v| v > 0 && v <= 12)
        && is_ok_and(&u32::from_str(&day.value), |&v| v > 0 && v <= 31);
    let mut body = Column::new().spacing(8).width(Length::Fill);
    if let Some(p) = scan_progress {
        body = body
            .push(page::status(
                page::Status::Busy,
                format!("Rescanning… {:.2}%", p * 100.0),
            ))
            .push(ProgressBar::new(0.0..=1.0, p as f32).length(Length::Fill));
    } else {
        body = body.push(
            Row::new()
                .spacing(12)
                .align_y(Alignment::End)
                .push(field("Year", "2022", year, "rescan_year"))
                .push(field("Month", "12", month, "rescan_month"))
                .push(field("Day", "31", day, "rescan_day"))
                .push(page::plain_button(
                    if processing {
                        "Starting rescan…"
                    } else {
                        "Start rescan"
                    },
                    ActionKind::Secondary,
                    ready.then_some(SettingsEditMessage::Confirm),
                ))
                .wrap(),
        );
        for (show, error) in [
            (invalid_date, "Provided date is invalid"),
            (
                past_possible_height,
                "Provided date earlier than the node prune height",
            ),
            (future_date, "Provided date is in the future"),
        ] {
            if show {
                body = body.push(text(error).size(13).style(theme::text::error));
            }
        }
    }
    if success {
        body = body.push(page::status(
            page::Status::Ok,
            "Successfully rescanned the blockchain",
        ));
    }
    if recovery_running {
        body = body.push(page::muted(
            "Available when wallet history recovery finishes or is cancelled.",
        ));
    }
    body = body.push(page::muted(
        "Re-reads blocks your node still keeps to rebuild this Vault's history. It does not repair Connect sign-in or node connectivity.",
    ));
    page::group("Rescan from a date", None, body)
}

fn is_ok_and<T, E>(res: &Result<T, E>, f: impl FnOnce(&T) -> bool) -> bool {
    if let Ok(v) = res {
        f(v)
    } else {
        false
    }
}

#[allow(clippy::too_many_arguments)]
pub const VAULT_NAME_ID: &str = "tenshu.settings.vault.name";
pub const VAULT_RECOVERY_TOOLS_ID: &str = "tenshu.settings.vault.recovery-tools";
pub const VAULT_PAIR_ID: &str = "tenshu.settings.vault.pair";
pub const VAULT_ADVANCED_ID: &str = "tenshu.settings.vault.advanced";
pub const VAULT_COPY_ID: &str = "tenshu.settings.vault.descriptor-copy";
pub const VAULT_EXPORT_ID: &str = "tenshu.settings.vault.descriptor-export";
pub const VAULT_REGISTER_ID: &str = "tenshu.settings.vault.register";
pub const VAULT_TOOLS_ID: &str = "tenshu.settings.vault.tools";

/// A signer's display name: its alias, else its fingerprint.
fn signer_name(
    fingerprint: &Fingerprint,
    keys_aliases: &[(Fingerprint, form::Value<String>)],
) -> String {
    keys_aliases
        .iter()
        .find(|(fg, alias)| fg == fingerprint && !alias.value.is_empty())
        .map(|(_, alias)| alias.value.clone())
        .unwrap_or_else(|| fingerprint.to_string())
}

/// "N of M signers approve" or "Alice approves", naming each signer.
fn quorum_sentence(
    threshold: usize,
    keys: &[Fingerprint],
    keys_aliases: &[(Fingerprint, form::Value<String>)],
    verb: &str,
) -> String {
    let names: Vec<String> = keys.iter().map(|k| signer_name(k, keys_aliases)).collect();
    if keys.len() == 1 {
        format!("{} {}s.", names[0], verb)
    } else {
        format!(
            "Any {} of {} signers {}: {}.",
            threshold,
            keys.len(),
            verb,
            names.join(", ")
        )
    }
}

/// The policy as artboard rows (`#h-vid`): normal spending, then one row per
/// recovery path with its timelock.
fn policy_rows<'a>(
    policy: CoincubePolicy,
    keys_aliases: &'a [(Fingerprint, form::Value<String>)],
    provider_keys: &'a HashMap<Fingerprint, ProviderKey>,
) -> Vec<Element<'a, Message>> {
    use crate::app::view::settings::page;
    let (primary_threshold, primary_keys) = policy.primary_path().thresh_origins();
    // HashMap order changes between renders; sort for a stable sentence.
    let mut primary_keys: Vec<Fingerprint> = primary_keys.into_keys().collect();
    primary_keys.sort();
    let mut rows: Vec<Element<'a, Message>> = vec![page::row("Normal spending")
        .description(quorum_sentence(
            primary_threshold,
            &primary_keys,
            keys_aliases,
            "approve",
        ))
        .into()];
    let recovery_paths = policy.recovery_paths();
    let several = recovery_paths.len() > 1;
    for (i, (sequence, recovery_path)) in recovery_paths.iter().enumerate() {
        let (threshold, recovery_keys) = recovery_path.thresh_origins();
        let mut recovery_keys: Vec<Fingerprint> = recovery_keys.into_keys().collect();
        recovery_keys.sort();
        let label = recovery_path_label(i, several, *sequence, &recovery_keys, provider_keys);
        let detail = format!(
            "After {} blocks (~{}) without spending. {}",
            sequence,
            expire_message_units(*sequence as u32).join(","),
            quorum_sentence(threshold, &recovery_keys, keys_aliases, "spend")
                .replace("signers spend", "signers can spend")
                .replace("spends.", "can spend."),
        );
        rows.push(
            page::row(label)
                .description(detail)
                .control_maybe((i == 0).then(|| {
                    page::button(
                        VAULT_RECOVERY_TOOLS_ID,
                        "Open recovery tools",
                        Some(Message::Menu(Menu::Vault(VaultSubMenu::Recovery))),
                    )
                }))
                .into(),
        );
    }
    rows
}

/// A recovery path's row label, shared by the policy and signer rows so
/// they name a path the same way. A maximum timelock held only by provider
/// keys is the Safety Net.
fn recovery_path_label(
    index: usize,
    several: bool,
    sequence: u16,
    keys: &[Fingerprint],
    provider_keys: &HashMap<Fingerprint, ProviderKey>,
) -> String {
    if sequence == u16::MAX && keys.iter().all(|fg| provider_keys.contains_key(fg)) {
        "Safety Net path".to_string()
    } else if several {
        format!("Recovery path #{}", index + 1)
    } else {
        "Recovery path".to_string()
    }
}

/// Which spending paths a signer belongs to, named as the policy rows are.
fn signer_paths(
    policy: &CoincubePolicy,
    fingerprint: &Fingerprint,
    provider_keys: &HashMap<Fingerprint, ProviderKey>,
) -> String {
    let mut paths = Vec::new();
    if policy
        .primary_path()
        .thresh_origins()
        .1
        .contains_key(fingerprint)
    {
        paths.push("Normal spending".to_string());
    }
    let recovery_paths = policy.recovery_paths();
    let several = recovery_paths.len() > 1;
    for (i, (sequence, path)) in recovery_paths.iter().enumerate() {
        let keys = path.thresh_origins().1;
        if keys.contains_key(fingerprint) {
            let mut keys: Vec<Fingerprint> = keys.into_keys().collect();
            keys.sort();
            paths.push(recovery_path_label(
                i,
                several,
                *sequence,
                &keys,
                provider_keys,
            ));
        }
    }
    paths.join(", ")
}

#[allow(clippy::too_many_arguments)]
pub fn wallet_settings<'a>(
    menu: &'a Menu,
    cache: &'a Cache,
    descriptor: &'a WalletDescriptor,
    vault_id: Fingerprint,
    wallet_alias: &'a form::Value<String>,
    keys_aliases: &'a [(Fingerprint, form::Value<String>)],
    provider_keys: &'a HashMap<Fingerprint, ProviderKey>,
    processing: bool,
    updated: bool,
    hardware_registration_allowed: bool,
    expert_open: bool,
) -> Element<'a, Message> {
    use crate::app::view::settings::page;
    use coincube_ui::component::tenshu::{self as tenshu_ui, FieldSize};
    let vault = descriptor.as_vault();
    let save =
        (!processing && wallet_alias.valid).then_some(Message::Settings(SettingsMessage::Save));

    let name_field = tenshu_ui::text_field(
        VAULT_NAME_ID,
        if vault.is_some() {
            "Vault name"
        } else {
            "Wallet name"
        },
        &wallet_alias.value,
        Message::FocusAction,
    )
    .hide_label()
    .size(FieldSize::Compact)
    .width(Length::Fixed(280.0))
    .error_maybe((!wallet_alias.valid).then_some("Please enter a shorter name"))
    .on_input(|value| Message::Settings(SettingsMessage::WalletAliasEdited(value)));
    let name_field = match save.clone() {
        Some(save) => name_field.on_submit(save),
        None => name_field,
    };

    let mut identity: Vec<Element<'a, Message>> = vec![page::row(if vault.is_some() {
        "Vault name"
    } else {
        "Wallet name"
    })
    .description(format!(
        "{} ID {} · {}",
        if vault.is_some() { "Vault" } else { "Wallet" },
        vault_id,
        crate::app::view::nav::shell::network_label(cache.chain())
    ))
    .control(Container::new(name_field).width(Length::Fixed(280.0)))
    .into()];
    if let Some(vault) = vault {
        identity.extend(policy_rows(vault.policy(), keys_aliases, provider_keys));
    }

    let mut content = Column::new().width(Length::Fill);
    if vault.is_none() {
        content = content.push(page::note(
            coincube_ui::component::tenshu::Tone::Muted,
            Row::new()
                .spacing(4)
                .push(text("Imported wallet · no local keys.").size(13).bold())
                .push(text("Signing needs the external signers below.").size(13))
                .wrap(),
            None,
        ));
    }
    content = content.push(page::group(
        "Identity and policy",
        None,
        page::rows(identity),
    ));

    let policy = vault.map(|v| v.policy());
    let signer_rows = keys_aliases.iter().map(|(fingerprint, name)| {
        let fg = *fingerprint;
        let mut detail = fg.to_string();
        if let Some(policy) = &policy {
            let paths = signer_paths(policy, &fg, provider_keys);
            if !paths.is_empty() {
                detail = format!("{} · {}", detail, paths);
            }
        }
        if provider_keys.contains_key(&fg) {
            detail = format!("{} · Provider key", detail);
        }
        let field = tenshu_ui::text_field(
            iced::advanced::widget::Id::from(format!("tenshu.settings.vault.signer.{}", fg)),
            "Signer name",
            &name.value,
            Message::FocusAction,
        )
        .hide_label()
        .size(FieldSize::Compact)
        .width(Length::Fixed(280.0))
        .error_maybe((!name.valid).then_some("Please enter correct alias"))
        .on_input(move |value| {
            Message::Settings(SettingsMessage::FingerprintAliasEdited(fg, value))
        });
        let field = match save.clone() {
            Some(save) => field.on_submit(save),
            None => field,
        };
        page::row(signer_name(&fg, keys_aliases))
            .line(tenshu_ui::mono(detail))
            .control(Container::new(field).width(Length::Fixed(280.0)))
            .into()
    });
    content = content
        .push(page::group(
            "Signers",
            Some("Names save when you press Return.".into()),
            page::rows(signer_rows),
        ))
        .push(page::saved_line(updated));

    // A Vault without a Keychain key has no Keychain tab to open.
    let pairable = vault.is_some() && !cache.vault_lacks_keychain_key;
    let pair_row = page::row("Keychain").description(if pairable {
        "Pair a phone holding this Vault's Keychain key. Same Wi-Fi. No Connect account needed."
    } else if vault.is_some() {
        "Unavailable: this Vault has no Keychain key. One can only be added when a Vault is \
         created."
    } else {
        "Unavailable: this wallet has no Keychain-eligible key in Tenshu."
    });
    let pair_row = if pairable {
        pair_row.control(page::button(
            VAULT_PAIR_ID,
            "Open Keychain",
            Some(Message::Menu(Menu::Vault(VaultSubMenu::Settings(Some(
                crate::app::menu::SettingsOption::LocalSigning,
            ))))),
        ))
    } else {
        pair_row.control(page::status(page::Status::Off, "Unavailable"))
    };
    content = content.push(page::group(
        "Phone signing and members",
        None,
        page::rows([
            pair_row.into(),
            page::row("Members")
                .accessory(page::status(page::Status::Off, "Not enabled"))
                .description("For eligible Connect Vaults only.")
                .into(),
        ]),
    ));

    content = content.push(page::disclosure(
        VAULT_ADVANCED_ID,
        "Advanced",
        Some("· descriptor, keys, hardware"),
        expert_open,
        Message::Settings(SettingsMessage::ToggleWalletExpert),
    ));
    if expert_open {
        let mut descriptor_row = page::row("Descriptor")
            .description("Hidden. Copying exposes the wallet policy and public keys to other apps.")
            .control(page::button(
                VAULT_COPY_ID,
                "Copy…",
                Some(Message::Settings(SettingsMessage::ReviewDescriptorCopy)),
            ));
        if vault.is_some() {
            descriptor_row = descriptor_row.control(page::button(
                VAULT_EXPORT_ID,
                "Export encrypted…",
                Some(Message::Settings(
                    SettingsMessage::ExportEncryptedDescriptor,
                )),
            ));
        }
        let mut advanced: Vec<Element<'a, Message>> = vec![descriptor_row.into()];
        if hardware_registration_allowed {
            advanced.push(
                page::row("Hardware registration")
                    .description("Register this wallet on a hardware device. Re-register after policy changes.")
                    .control(page::button(
                        VAULT_REGISTER_ID,
                        "Register on device…",
                        Some(Message::Settings(SettingsMessage::RegisterWallet)),
                    ))
                    .into(),
            );
        }
        if vault.is_some() {
            advanced.push(
                page::row("Recovery path, coin control and PSBTs")
                    .control(page::button(
                        VAULT_TOOLS_ID,
                        "Open Vault tools",
                        Some(Message::Menu(Menu::Vault(VaultSubMenu::Recovery))),
                    ))
                    .into(),
            );
        }
        content = content.push(page::disclosure_body(advanced));
    }

    dashboard(menu, cache, content)
}

pub fn descriptor_copy_review<'a>() -> Element<'a, Message> {
    card::modal(
        Column::new()
            .spacing(15)
            .push(h4_bold("Copy wallet descriptor?"))
            .push(text(
                "The descriptor reveals this wallet's policy and public keys. Other apps may read the clipboard. Copy only if you trust where you will paste it.",
            ))
            .push(
                Row::new()
                    .spacing(10)
                    .push(
                        button::secondary(None, "Cancel")
                            .on_press(Message::Settings(SettingsMessage::CancelDescriptorCopy)),
                    )
                    .push(
                        button::primary(None, "Copy descriptor")
                            .on_press(Message::Settings(SettingsMessage::ConfirmDescriptorCopy)),
                    ),
            ),
    )
    .into()
}

/// returns y,m,d
fn expire_message_units(sequence: u32) -> Vec<String> {
    let mut n_minutes = sequence * 10;
    let n_years = n_minutes / 525960;
    n_minutes -= n_years * 525960;
    let n_months = n_minutes / 43830;
    n_minutes -= n_months * 43830;
    let n_days = n_minutes / 1440;

    #[allow(clippy::nonminimal_bool)]
    if n_years != 0 || n_months != 0 || n_days != 0 {
        [(n_years, "y"), (n_months, "m"), (n_days, "d")]
            .iter()
            .filter_map(|(n, u)| {
                if *n != 0 {
                    Some(format!("{}{}", n, u))
                } else {
                    None
                }
            })
            .collect()
    } else {
        n_minutes -= n_days * 1440;
        let n_hours = n_minutes / 60;
        n_minutes -= n_hours * 60;
        [(n_hours, "h"), (n_minutes, "m")]
            .iter()
            .filter_map(|(n, u)| {
                if *n != 0 {
                    Some(format!("{}{}", n, u))
                } else {
                    None
                }
            })
            .collect()
    }
}

#[allow(clippy::too_many_arguments)]
pub fn node_backend_status<'a>(
    active_backend: &'static str,
    active_icon: coincube_ui::widget::Text<'static>,
    pending_progress: Option<f64>,
    local_heights: Option<(u64, u64)>,
    pending_ibd: Option<bool>,
    // `getnetworkinfo.subversion` of the syncing node, when it has told us.
    // `None` leaves the build unnamed rather than asserting one.
    pending_subversion: Option<&'a str>,
    pending_bitcoind_log: Option<&'a str>,
    show_log: bool,
    can_switch_to_connect: bool,
    can_switch_to_bitcoind: bool,
    recovery_in_progress: bool,
    can_setup_local_node: bool,
    auto_switch_to_pending: bool,
    processing: bool,
    switch_in_progress: bool,
    warning: Option<String>,
) -> Element<'a, NodeSettingsMessage> {
    // A backend switch (manual or the background auto-promotion) is running.
    // Treat it like `processing` so the switch buttons are disabled rather
    // than offering an action that just errors "already in progress".
    let processing = processing || switch_in_progress;
    // The estimate can reach (or round to) 100% before the node is ready.
    // Completion requires both leaving IBD and verifying every known header.
    let initial_syncing = pending_ibd != Some(false);
    // Leaving IBD does not mean a previously synced node has caught up after
    // an outage. Keep its known header gap visible without changing admission.
    let catching_up =
        !initial_syncing && local_heights.is_some_and(|(blocks, headers)| blocks < headers);
    let still_syncing =
        initial_syncing || !local_heights.is_some_and(|(blocks, headers)| blocks >= headers);
    use crate::app::view::settings::page;
    use coincube_ui::component::tenshu::{ActionKind, Tone};
    let _ = active_icon;
    let mut col = Column::new().width(Length::Fill);

    // Surface an in-flight backend switch so the disabled buttons read as
    // "busy" rather than broken. This covers the promotion/rescan window
    // after the pending node reports synced. Keep blockchain progress visible
    // separately from the wallet switch so neither status obscures the other.
    if switch_in_progress {
        col = col.push(page::note(
            Tone::Muted,
            Column::new()
                .spacing(2)
                .push(page::status(
                    page::Status::Busy,
                    "Applying your node change…",
                ))
                .push(page::muted(
                    "Tenshu is stopping the previous wallet backend and loading the new one. \
                     This can take a while if a wallet scan is still running. \
                     Local blockchain progress is shown separately below.",
                )),
            None,
        ));
    }

    // Actions on the backend in use sit on its row; switching to the local
    // node sits on the node's row.
    let mut actions: Vec<Element<'a, NodeSettingsMessage>> = Vec::new();
    let mut node_actions: Vec<Element<'a, NodeSettingsMessage>> = Vec::new();
    if can_switch_to_connect {
        actions.push(page::plain_button(
            "Switch to COINCUBE | Connect",
            ActionKind::Secondary,
            (!processing).then_some(NodeSettingsMessage::SwitchToConnect),
        ));
    }
    // A switch cannot run during wallet history recovery or while the pending
    // node is in IBD, so omit its button in those states. When IBD status is
    // still unknown, keep the button visible: the action guard can explain
    // why switching is not yet available.
    let show_switch_to_bitcoind_btn =
        can_switch_to_bitcoind && pending_ibd != Some(true) && !recovery_in_progress;
    if show_switch_to_bitcoind_btn {
        node_actions.push(page::plain_button(
            "Switch to local node",
            ActionKind::Secondary,
            (!processing).then_some(NodeSettingsMessage::SwitchToBitcoind),
        ));
    }
    if can_setup_local_node {
        actions.push(page::plain_button(
            "Set up local node",
            ActionKind::Secondary,
            (!processing).then_some(NodeSettingsMessage::SetupLocalNode),
        ));
    }

    // A pending node is not necessarily one of ours: switching to Connect
    // parks whichever Bitcoind backend was active, and that can be an
    // external node running any client. So the name is only used for a
    // subversion we actually recognise.
    let flavor = pending_subversion.and_then(|sv| {
        let flavor = crate::node::bitcoind::recognized_flavor(sv)?.display_name();
        Some(match crate::node::bitcoind::node_version_label(sv) {
            Some(version) => format!("{flavor} {version}"),
            None => flavor.to_string(),
        })
    });
    let name = flavor
        .clone()
        .unwrap_or_else(|| "Your local node".to_string());
    let mut status_block: Option<coincube_ui::widget::Column<'a, NodeSettingsMessage>> = None;
    if let Some(progress) = pending_progress {
        let percent = crate::utils::syncing_percent(progress);
        let pace = if progress > 0.98 {
            "This may take a few minutes, depending on the last time it was done, your internet connection, and your computer performance."
        } else if progress > 0.9 {
            "This will take a while, depending on the last time it was done, your internet connection, and your computer performance."
        } else {
            "A full synchronisation typically takes a few days and is resource-intensive. Once the initial synchronisation is done, the next ones will be much faster."
        };
        let pending = can_switch_to_bitcoind;
        let desc = if catching_up {
            Some(format!("{name} is catching up with known headers."))
        } else if still_syncing {
            let next = if pending && auto_switch_to_pending {
                "Tenshu will automatically switch to this node once syncing is complete. "
            } else {
                ""
            };
            Some(format!("{name} is syncing the blockchain. {next}{pace}"))
        } else if pending && recovery_in_progress {
            Some("Bring this Vault's history over below, then switch.".to_string())
        } else if pending {
            (!switch_in_progress)
                .then(|| "Ready to switch this Vault to your local node.".to_string())
        } else {
            None
        };

        let mut block = Column::new().spacing(6).width(Length::Fill);
        if still_syncing {
            block = block
                .push(page::status(
                    page::Status::Busy,
                    format!(
                        "{} {:.1}%",
                        if catching_up {
                            "Catching up"
                        } else {
                            "Syncing"
                        },
                        percent
                    ),
                ))
                .push(ProgressBar::new(0.0..=100.0, percent as f32).length(Length::Fill));
        }
        if let Some((blocks, headers)) = local_heights {
            block = block.push(
                text(format!(
                    "Verified blocks: {blocks} / {headers} known headers · {} blocks remaining",
                    headers.saturating_sub(blocks),
                ))
                .size(14),
            );
        }
        if let Some(desc) = desc {
            block = block.push(page::muted(desc));
        }
        if let Some(log) = pending_bitcoind_log.filter(|_| show_log) {
            block = block.push(coincube_ui::component::tenshu::mono(log).size(12));
        }
        status_block = Some(block);
    } else if can_switch_to_bitcoind || active_backend == "Local Node" {
        let mut block = Column::new().spacing(6).push(page::status(
            page::Status::Busy,
            "Waiting for the local node to report sync progress…",
        ));
        // A starting node's log is most useful before it reports progress.
        if let Some(log) = pending_bitcoind_log.filter(|_| show_log) {
            block = block.push(coincube_ui::component::tenshu::mono(log).size(12));
        }
        status_block = Some(block);
    }

    let in_use = match active_backend {
        "COINCUBE | Connect" => "This Vault reads its balance and history through Connect.",
        "Local Node" => "This Vault reads its balance and history from your own node.",
        _ => "Where this Vault reads its balance and history.",
    };
    let mut rows: Vec<Element<'a, NodeSettingsMessage>> = vec![actions
        .into_iter()
        .fold(
            page::row(format!("In use · {active_backend}")).description(in_use),
            |row, action| row.control(action),
        )
        .into()];
    if let Some(block) = status_block {
        if pending_bitcoind_log.is_some() {
            node_actions.push(page::plain_button(
                if show_log {
                    "Hide node log"
                } else {
                    "Node log"
                },
                ActionKind::Secondary,
                Some(NodeSettingsMessage::NodeLogToggled),
            ));
        }
        let label = match &flavor {
            Some(flavor) => format!("Local node · {flavor}"),
            None => "Local node".to_string(),
        };
        let mut node_row = page::row(label);
        if pending_progress.is_some() && !still_syncing {
            node_row =
                node_row.accessory(page::status(page::Status::Ok, "Blockchain sync complete"));
        }
        rows.push(
            node_actions
                .into_iter()
                .fold(node_row.line(block).top(), |row, action| {
                    row.control(action)
                })
                .into(),
        );
    }
    col = col.push(page::group("Wallet backend", None, page::rows(rows)));

    if let Some(w) = warning {
        col = col.push(page::note(Tone::Warn, text(w).size(13), None));
    }

    col.into()
}

pub fn pending_node_setup_panel<'a>(
    addr: &'a form::Value<String>,
    rpc_auth_vals: &'a RpcAuthValues,
    selected_auth_type: &'a RpcAuthType,
    processing: bool,
) -> Element<'a, NodeSettingsMessage> {
    let mut col = Column::new().spacing(15);

    col = col.push(
        Row::new()
            .push(
                text("Set up local node")
                    .bold()
                    .size(18)
                    .width(Length::Fill),
            )
            .push(Space::new().width(Length::Fill))
            .push(
                button::transparent(None, "Cancel")
                    .on_press(NodeSettingsMessage::SetupLocalNodeCancel),
            )
            .align_y(Alignment::Center),
    );

    col = col.push(
        Column::new()
            .spacing(8)
            .push(caption("Bitcoin Core RPC address"))
            .push(
                TextInput::new("127.0.0.1:8332", &addr.value)
                    .on_input(NodeSettingsMessage::SetupLocalNodeAddrChanged)
                    .size(16)
                    .padding(12)
                    .width(Length::Fill),
            )
            .push(if addr.valid {
                text("").size(12)
            } else {
                text("Invalid address (expected host:port)")
                    .style(theme::text::warning)
                    .size(12)
            }),
    );

    col = col.push(
        Column::new()
            .spacing(8)
            .push(caption("Authentication"))
            .push(
                Row::new()
                    .spacing(15)
                    .push(radio(
                        "Cookie file",
                        RpcAuthType::CookieFile,
                        Some(*selected_auth_type),
                        NodeSettingsMessage::SetupLocalNodeAuthTypeSelected,
                    ))
                    .push(radio(
                        "Username / Password",
                        RpcAuthType::UserPass,
                        Some(*selected_auth_type),
                        NodeSettingsMessage::SetupLocalNodeAuthTypeSelected,
                    )),
            ),
    );

    match selected_auth_type {
        RpcAuthType::CookieFile => {
            col = col.push(
                Column::new()
                    .spacing(8)
                    .push(caption("Cookie file path"))
                    .push(
                        TextInput::new(
                            "/path/to/.bitcoin/.cookie",
                            &rpc_auth_vals.cookie_path.value,
                        )
                        .on_input(|v| {
                            NodeSettingsMessage::SetupLocalNodeFieldEdited("cookie_file_path", v)
                        })
                        .size(16)
                        .padding(12)
                        .width(Length::Fill),
                    )
                    .push(if rpc_auth_vals.cookie_path.valid {
                        text("").size(12)
                    } else {
                        text("Cookie file path must not be empty")
                            .style(theme::text::warning)
                            .size(12)
                    }),
            );
        }
        RpcAuthType::UserPass => {
            col = col.push(
                Row::new()
                    .spacing(10)
                    .push(
                        Column::new()
                            .spacing(8)
                            .push(caption("Username"))
                            .push(
                                TextInput::new("rpcuser", &rpc_auth_vals.user.value)
                                    .on_input(|v| {
                                        NodeSettingsMessage::SetupLocalNodeFieldEdited("user", v)
                                    })
                                    .size(16)
                                    .padding(12)
                                    .width(Length::Fill),
                            )
                            .push(if rpc_auth_vals.user.valid {
                                text("").size(12)
                            } else {
                                text("Username must not be empty")
                                    .style(theme::text::warning)
                                    .size(12)
                            })
                            .width(Length::FillPortion(1)),
                    )
                    .push(
                        Column::new()
                            .spacing(8)
                            .push(caption("Password"))
                            .push(
                                TextInput::new("rpcpassword", &rpc_auth_vals.password.value)
                                    .on_input(|v| {
                                        NodeSettingsMessage::SetupLocalNodeFieldEdited(
                                            "password", v,
                                        )
                                    })
                                    .secure(true)
                                    .size(16)
                                    .padding(12)
                                    .width(Length::Fill),
                            )
                            .push(if rpc_auth_vals.password.valid {
                                text("").size(12)
                            } else {
                                text("Password must not be empty")
                                    .style(theme::text::warning)
                                    .size(12)
                            })
                            .width(Length::FillPortion(1)),
                    ),
            );
        }
    }

    let confirm_btn = button::primary(
        None,
        if processing {
            "Saving…"
        } else {
            "Add local node"
        },
    )
    .padding([10, 20]);
    col = col.push(if processing {
        confirm_btn
    } else {
        confirm_btn.on_press(NodeSettingsMessage::SetupLocalNodeConfirm)
    });

    card::simple(Container::new(col).padding(15).width(Length::Fill)).into()
}

pub fn node_setup_mode_picker_panel<'a>() -> Element<'a, NodeSettingsMessage> {
    let mut col = Column::new().spacing(15);

    col = col.push(
        Row::new()
            .push(
                text("Set up local node")
                    .bold()
                    .size(18)
                    .width(Length::Fill),
            )
            .push(
                button::transparent(None, "Cancel")
                    .on_press(NodeSettingsMessage::SetupLocalNodeCancel),
            )
            .align_y(Alignment::Center),
    );

    col = col.push(
        text("How would you like to connect to Bitcoin Core?")
            .style(theme::text::secondary)
            .size(14),
    );

    let external_card = card::simple(
        Container::new(
            Column::new()
                .spacing(8)
                .push(
                    Row::new()
                        .spacing(10)
                        .push(badge::badge(icon::network_icon()))
                        .push(text("My own node").bold())
                        .align_y(Alignment::Center),
                )
                .push(
                    text("Connect to a Bitcoin Core node you already have running.")
                        .size(13)
                        .style(theme::text::secondary),
                )
                .push(
                    button::secondary(None, "Configure →")
                        .padding([8, 14])
                        .on_press(NodeSettingsMessage::SetupLocalNodeModeSelected(false)),
                ),
        )
        .padding(15)
        .width(Length::Fill),
    );

    let internal_card = card::simple(
        Container::new(
            Column::new()
                .spacing(8)
                .push(
                    Row::new()
                        .spacing(10)
                        .push(badge::badge(icon::bitcoin_icon()))
                        .push(text("Tenshu-managed node (recommended)").bold())
                        .align_y(Alignment::Center),
                )
                .push(
                    text("Tenshu downloads and runs a pruned node (~15 GB) automatically. No configuration needed.")
                        .size(13)
                        .style(theme::text::secondary),
                )
                .push(
                    Row::new()
                        .spacing(10)
                        .push(
                            button::primary(None, "Bitcoin Knots →")
                                .padding([8, 14])
                                .on_press(NodeSettingsMessage::SetupLocalNodeManagedFlavor(
                                    NodeFlavor::Knots,
                                )),
                        )
                        .push(
                            button::secondary(None, "Bitcoin Core →")
                                .padding([8, 14])
                                .on_press(NodeSettingsMessage::SetupLocalNodeManagedFlavor(
                                    NodeFlavor::Core,
                                )),
                        ),
                )
                .push(
                    text(
                        "Knots ships stricter relay defaults — it limits oversized \
                         data-carrier (\"spam\") transactions. Standard sends are unaffected.",
                    )
                    .size(12)
                    .style(theme::text::secondary),
                )
                .push(
                    text(
                        "Your Vaults share one Bitcoin node, so this applies to every Vault — \
                         switching restarts the node and moves them all to the chosen build.",
                    )
                    .size(12)
                    .style(theme::text::secondary),
                ),
        )
        .padding(15)
        .width(Length::Fill),
    );

    col = col.push(external_card);
    col = col.push(internal_card);

    card::simple(Container::new(col).padding(15).width(Length::Fill)).into()
}

pub fn internal_node_setup_panel<'a>(
    flavor: NodeFlavor,
    downloading: bool,
    installing: bool,
    done: bool,
    error: Option<&'a str>,
    download_progress: f32,
) -> Element<'a, NodeSettingsMessage> {
    let name = flavor.display_name();
    let mut col = Column::new().spacing(15);

    col = col.push(
        Row::new()
            .push(
                text("Setting up local node")
                    .bold()
                    .size(18)
                    .width(Length::Fill),
            )
            .push({
                let close_btn: Element<'_, NodeSettingsMessage> = if done || error.is_some() {
                    button::transparent(None, "Close")
                        .on_press(NodeSettingsMessage::SetupLocalNodeCancel)
                        .into()
                } else {
                    Space::new().into()
                };
                close_btn
            })
            .align_y(Alignment::Center),
    );

    if let Some(err) = error {
        col = col.push(
            Column::new()
                .spacing(8)
                .push(text("Setup failed").bold().style(theme::text::warning))
                .push(text(err).size(13).style(theme::text::warning))
                .push(
                    Row::new()
                        .spacing(10)
                        .push(
                            button::secondary(None, "Retry")
                                .padding([8, 14])
                                .on_press(NodeSettingsMessage::SetupLocalNodeModeSelected(true)),
                        )
                        .push(
                            button::transparent(None, "Cancel")
                                .on_press(NodeSettingsMessage::SetupLocalNodeCancel),
                        ),
                ),
        );
    } else if done {
        col = col.push(
            Column::new()
                .spacing(6)
                .push(text(format!("{name} is running and syncing in the background.")).bold())
                .push(
                    text("Tenshu will automatically switch to this node once syncing is complete.")
                        .size(13)
                        .style(theme::text::secondary),
                ),
        );
    } else if downloading {
        col = col.push(
            Column::new()
                .spacing(10)
                .push(
                    text(format!(
                        "Downloading {name}… {:.0}%",
                        download_progress.min(99.9)
                    ))
                    .size(14),
                )
                .push(coincube_ui::widget::ProgressBar::new(
                    0.0..=100.0,
                    download_progress,
                )),
        );
    } else if installing {
        col = col.push(
            text(format!("Installing and starting {name}…"))
                .size(14)
                .style(theme::text::secondary),
        );
    }

    card::simple(Container::new(col).padding(15).width(Length::Fill)).into()
}

/// The "Inbound connections" panel: opt in/out of inbound-over-Tor and tune it.
/// Each control persists the preference sidecar; the change takes effect the
/// next time the managed node starts.
/// Format a byte count for display (`45 MB`, `1.0 GB`).
fn human_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.0} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.0} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

fn node_message(message: NodeSettingsMessage) -> Message {
    Message::Settings(SettingsMessage::NodeSettings(message))
}

pub const TOR_INBOUND_ID: &str = "tenshu.settings.node.tor-inbound";
pub const TOR_LIMIT_ID: &str = "tenshu.settings.node.tor-limit";
pub const TOR_OUTBOUND_ID: &str = "tenshu.settings.node.tor-outbound";
pub const NODE_RESTART_ID: &str = "tenshu.settings.node.restart";
pub const NODE_RESOURCES_APPLY_ID: &str = "tenshu.settings.node.resources-apply";

pub fn inbound_tor_section<'a>(
    enabled: bool,
    outbound_via_tor: bool,
    limit_upload: bool,
    tor_running: bool,
    onion_active: bool,
    supported: bool,
    stats: Option<&crate::app::cache::NodeNetStats>,
) -> Element<'a, Message> {
    use crate::app::view::settings::page;
    use coincube_ui::component::tenshu::toggle;
    let intro = "Let other peers reach your node over Tor — a private onion service, no \
                 port-forwarding needed. Your node then serves blocks and transactions to them, \
                 instead of only downloading from others.";
    if !supported {
        return page::group(
            "Inbound connections",
            Some(intro.into()),
            page::rows([page::row("Accept inbound connections over Tor")
                .control(page::status(
                    page::Status::Off,
                    "Not available on this platform",
                ))
                .into()]),
        );
    }

    let mut rows: Vec<Element<'a, Message>> =
        vec![page::row("Accept inbound connections over Tor")
            .control(
                toggle(TOR_INBOUND_ID, enabled, Message::FocusAction)
                    .on_toggle(|on| node_message(NodeSettingsMessage::InboundTorToggled(on))),
            )
            .into()];
    if enabled {
        rows.push(
            page::row("Limit upload to ~1 GB/day")
                .description("Uploads up to ~1 GB/day. Turn off the limit only if your internet connection isn't metered.")
                .control(
                    toggle(TOR_LIMIT_ID, limit_upload, Message::FocusAction).on_toggle(|on| {
                        node_message(NodeSettingsMessage::InboundTorLimitUploadToggled(on))
                    }),
                )
                .into(),
        );
        rows.push(
            page::row("Also route outbound connections through Tor")
                .control(
                    toggle(TOR_OUTBOUND_ID, outbound_via_tor, Message::FocusAction).on_toggle(
                        |on| node_message(NodeSettingsMessage::InboundTorOutboundToggled(on)),
                    ),
                )
                .into(),
        );
    }

    // Live participation stats — visible whether or not inbound is on, so the
    // user can see their node is actually taking part in the network.
    if let Some(s) = stats {
        rows.push(
            page::row("Connections")
                .control(
                    text(format!(
                        "{} inbound · {} outbound",
                        s.connections_in, s.connections_out
                    ))
                    .size(14),
                )
                .into(),
        );
        if let Some(onion) = &s.onion_address {
            rows.push(
                page::row("Onion address")
                    .control(
                        Row::new()
                            .spacing(8)
                            .align_y(Alignment::Center)
                            .push(coincube_ui::component::tenshu::mono(onion.clone()))
                            .push(
                                Button::new(icon::clipboard_icon().size(14))
                                    .style(theme::button::transparent_border)
                                    .on_press(node_message(NodeSettingsMessage::CopyToClipboard(
                                        onion.clone(),
                                    ))),
                            ),
                    )
                    .into(),
            );
        }
        if enabled {
            let cap = if s.upload_target > 0 {
                human_bytes(s.upload_target)
            } else {
                "unlimited".to_string()
            };
            rows.push(
                page::row("Shared today")
                    .control(text(format!("{} of {}", human_bytes(s.upload_used), cap)).size(14))
                    .into(),
            );
        }
    }

    // Status + apply-now. Every change above (including turning the feature
    // off) only reaches the running node when it restarts.
    let (kind, status) = if tor_running && onion_active {
        // Tor is up AND bitcoind has an onion key it re-publishes on start.
        (
            page::Status::Ok,
            "Tor is running — your node is reachable as an onion service.",
        )
    } else if tor_running && enabled {
        (
            page::Status::Warn,
            "Tor is running — restart the node to publish your onion service.",
        )
    } else if enabled {
        (
            page::Status::Warn,
            "Not reachable over Tor yet — restart the node to apply.",
        )
    } else {
        (page::Status::Off, "Inbound over Tor is off.")
    };
    let mut content = Column::new()
        .width(Length::Fill)
        .push(page::rows(rows))
        .push(Space::new().height(Length::Fixed(12.0)))
        .push(
            Row::new()
                .spacing(16)
                .align_y(Alignment::Center)
                .push(Container::new(page::status(kind, status)).width(Length::Fill))
                .push(page::button(
                    NODE_RESTART_ID,
                    "Restart node to apply",
                    Some(node_message(NodeSettingsMessage::RestartNodeToApply)),
                )),
        );
    // Set expectations: the node only serves while the app is open and the
    // machine is awake.
    if enabled {
        content = content.push(page::caption(
            "Your node is reachable while Tenshu is open and your computer is awake. \
             Minimizing is fine; if the computer sleeps, connections reconnect \
             automatically on wake.",
        ));
    }
    page::group("Inbound connections", Some(intro.into()), content)
}

/// The "Your own Esplora" panel (`PLAN-connect-blinding` PR D5).
///
/// By default a Vault's chain queries go through COINCUBE's Esplora proxy or a
/// public provider, which means somebody other than the user sees which
/// addresses are being looked up (IP-linked, not account-linked — see
/// `SECURITY.md`). Pointing the daemon at the user's own Esplora removes that
/// party from the path entirely. The copy says exactly that, in those terms:
/// this is the mitigation we actually offer, so it shouldn't be undersold or
/// oversold.
///
/// `probing` is true while the entered URL is being checked; `status` carries
/// the last probe verdict (`Ok(tip_height)` or the failure reason).
pub fn custom_esplora_section<'a>(
    url: &'a form::Value<String>,
    probing: bool,
    status: Option<&'a Result<u32, String>>,
) -> Element<'a, NodeSettingsMessage> {
    let field = form::Form::new_trimmed("https://esplora.example.com/api", url, |v| {
        NodeSettingsMessage::CustomEsploraEdited(v)
    })
    .warning("Enter a full http:// or https:// URL")
    .size(P1_SIZE)
    .padding(10);

    let apply: Element<'_, NodeSettingsMessage> = if probing {
        button::secondary(None, "Checking…").padding([8, 14]).into()
    } else {
        button::secondary(None, "Check and use")
            .padding([8, 14])
            .on_press(NodeSettingsMessage::CustomEsploraApply)
            .into()
    };

    let mut col = Column::new()
        .spacing(15)
        .push(text("Your own Esplora").bold().size(18))
        .push(
            text(
                "By default, chain queries go through COINCUBE's proxy — which means we \
                 see which addresses your wallet asks about, though not who you are. \
                 Point this at your own Esplora server to remove us from that path.",
            )
            .size(12)
            .style(theme::text::secondary),
        )
        .push(
            Row::new()
                .spacing(10)
                .align_y(Alignment::Center)
                .push(Container::new(field).width(Length::Fill))
                .push(apply),
        );

    if let Some(status) = status {
        col = col.push(match status {
            Ok(height) => text(format!(
                "Reachable — reported block height {}. Switching your chain queries over.",
                height
            ))
            .size(12)
            .style(theme::text::success),
            Err(e) => text(e.clone()).size(12).style(theme::text::error),
        });
    }

    col = col.push(separation().width(Length::Fill)).push(
        text(
            "No fallback is kept: if your server is unreachable, syncing stops rather than \
             quietly sending your address lookups somewhere else. Switch back to \
             COINCUBE | Connect above to restore the default providers.",
        )
        .size(12)
        .style(theme::text::secondary),
    );

    card::simple(col).width(Length::Fill).into()
}

/// How a step of the move-to-local-node card is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepState {
    Done,
    Current,
    Stopped,
    Todo,
}

/// One numbered step: a 28px marker, the title with a right-hand note, and
/// the step's body below the title.
fn history_step<'a>(
    number: u8,
    state: StepState,
    title: &'a str,
    note: Option<String>,
    body: Option<Element<'a, NodeSettingsMessage>>,
) -> Element<'a, NodeSettingsMessage> {
    use coincube_ui::component::tenshu::Tone;
    let tone = match state {
        StepState::Done => Tone::Good,
        StepState::Current => Tone::Neutral,
        StepState::Stopped => Tone::Warn,
        StepState::Todo => Tone::Muted,
    };
    let glyph: Element<'a, NodeSettingsMessage> = match state {
        StepState::Done => icon::check_icon()
            .size(14)
            .style(move |t: &theme::Theme| theme::tenshu::text::tone(t, tone))
            .into(),
        StepState::Stopped => text("!")
            .size(13)
            .font(coincube_ui::font::SEMIBOLD)
            .style(move |t: &theme::Theme| theme::tenshu::text::tone(t, tone))
            .into(),
        _ => text(number.to_string())
            .size(13)
            .font(coincube_ui::font::SEMIBOLD)
            .style(move |t: &theme::Theme| {
                if state == StepState::Current {
                    iced::widget::text::Style {
                        color: Some(t.colors.shell.accent_ink),
                    }
                } else {
                    theme::tenshu::text::tone(t, tone)
                }
            })
            .into(),
    };
    let marker = Container::new(glyph)
        .width(Length::Fixed(28.0))
        .height(Length::Fixed(28.0))
        .center_x(Length::Fixed(28.0))
        .center_y(Length::Fixed(28.0))
        .style(move |t: &theme::Theme| {
            let shell = &t.colors.shell;
            let (color, width) = match state {
                StepState::Done => (shell.good, 1.0),
                StepState::Current => (shell.accent, 2.0),
                StepState::Stopped => (shell.warn, 2.0),
                StepState::Todo => (shell.line, 1.0),
            };
            iced::widget::container::Style {
                border: iced::Border {
                    color,
                    width,
                    radius: 14.0.into(),
                },
                ..Default::default()
            }
        });
    let title_text = text(title)
        .size(15)
        .font(
            if matches!(state, StepState::Current | StepState::Stopped) {
                coincube_ui::font::SEMIBOLD
            } else {
                coincube_ui::font::MEDIUM
            },
        )
        .style(move |t: &theme::Theme| {
            theme::tenshu::text::tone(
                t,
                if state == StepState::Todo {
                    Tone::Muted
                } else {
                    Tone::Neutral
                },
            )
        });
    let mut header = Row::new()
        .spacing(16)
        .align_y(Alignment::Center)
        .height(Length::Fixed(28.0))
        .push(Container::new(title_text).width(Length::Fill));
    if let Some(note) = note {
        header = header.push(text(note).size(13).style(move |t: &theme::Theme| {
            theme::tenshu::text::tone(
                t,
                if state == StepState::Done {
                    Tone::Good
                } else {
                    Tone::Muted
                },
            )
        }));
    }
    let mut column = Column::new().spacing(10).width(Length::Fill).push(header);
    if let Some(body) = body {
        column = column.push(body);
    }
    Row::new()
        .spacing(16)
        .align_y(Alignment::Start)
        .padding([12, 0])
        .push(marker)
        .push(column)
        .into()
}

/// A choice card for where history recovery starts.
fn history_source<'a>(
    title: &'a str,
    tag: Option<&'a str>,
    detail: &'a str,
    selected: bool,
    from_date: bool,
) -> Element<'a, NodeSettingsMessage> {
    use crate::app::view::settings::page;
    let mut heading = Row::new()
        .spacing(6)
        .align_y(Alignment::Center)
        .push(text(title).size(14).font(coincube_ui::font::SEMIBOLD));
    if let Some(tag) = tag {
        heading = heading.push(text(tag).size(12).font(coincube_ui::font::SEMIBOLD).style(
            |t: &theme::Theme| iced::widget::text::Style {
                color: Some(t.colors.shell.accent_ink),
            },
        ));
    }
    iced::widget::button(
        Column::new()
            .spacing(3)
            .push(heading)
            .push(page::muted(detail)),
    )
    .width(Length::Fill)
    .padding([12, 16])
    .style(move |t, status| {
        let mut style = theme::tenshu::control::segment(t, status, selected, 10.0);
        if !selected {
            style.border.color = t.colors.shell.line;
            style.border.width = 1.0;
        }
        style
    })
    .on_press(NodeSettingsMessage::RecoverySourceSelected(from_date))
    .into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalNodeSwitchStep {
    Waiting,
    Ready,
    Switching,
    Complete,
}

/// The card that moves a Vault from Connect to the local node: sync the
/// blockchain, bring the Vault's history over, then switch. All three steps
/// stay on screen in every state, so a switch that is not yet possible says
/// why instead of disappearing.
pub fn wallet_history_section<'a>(
    start: &'a form::Value<String>,
    status: &'a crate::app::node_history::Status,
    from_date: bool,
    date_picker_open: bool,
    node_height: Option<u64>,
    switch_step: LocalNodeSwitchStep,
) -> Element<'a, NodeSettingsMessage> {
    use crate::app::view::settings::page;
    use crate::node::history::Phase;
    use coincube_ui::component::tenshu::{ActionKind, Tone};

    let job = status.job.as_ref();
    let running = job.is_some_and(|job| {
        matches!(
            job.phase,
            Phase::FetchingForLoad | Phase::Downloading | Phase::Scanning | Phase::Reconciling
        )
    });
    let paused = job.is_some_and(|job| job.phase == Phase::Paused);
    let switched = switch_step == LocalNodeSwitchStep::Complete;
    let switching = switch_step == LocalNodeSwitchStep::Switching;
    let history_available = switch_step != LocalNodeSwitchStep::Waiting;
    let done = history_available
        || (status.reconciled && job.is_some_and(|job| job.phase == Phase::Complete));
    // A user action is in flight but no job has been written yet.
    let preparing = status.busy && !running && !paused && !done;
    let can_start = !status.busy
        && job.is_none_or(|job| {
            matches!(
                job.phase,
                Phase::Complete | Phase::Cancelled | Phase::Paused
            )
        });

    let step1 = match node_height {
        Some(height) => history_step(
            1,
            StepState::Done,
            "Sync the blockchain",
            Some(format!("Done · block {height}")),
            None,
        ),
        None if switching || switched => history_step(
            1,
            StepState::Done,
            "Sync the blockchain",
            Some("Done".into()),
            None,
        ),
        None => history_step(
            1,
            StepState::Current,
            "Sync the blockchain",
            Some("Waiting for your node".into()),
            None,
        ),
    };

    let percent = job.map(|job| job.progress() * 100.0).unwrap_or(0.0);
    let bar = |progress: f32| -> Element<'a, NodeSettingsMessage> {
        ProgressBar::new(0.0..=1.0, progress)
            .length(Length::Fill)
            .girth(Length::Fixed(6.0))
            .into()
    };
    let blocks_line = |job: &crate::node::history::Recovery| {
        if job.tail.is_some() {
            let (done, total) = job.block_counts();
            format!("{done} of {total} blocks")
        } else {
            format!("Blocks {}–{}", job.start, job.target)
        }
    };

    let mut body = Column::new().spacing(10).width(Length::Fill);
    let (state2, note2) = if done {
        body = body.push(page::note(
            Tone::Good,
            Column::new()
                .spacing(2)
                .push(page::status(
                    page::Status::Ok,
                    if status.reconciled {
                        "History matches your node"
                    } else {
                        "Vault history is available"
                    },
                ))
                .push(page::muted(if status.reconciled {
                    "Every payment and spend Connect found checks out against your node."
                } else {
                    "Your node can provide this Vault's history."
                })),
            None,
        ));
        (StepState::Done, Some("Done".to_string()))
    } else if preparing {
        body = body
            .push(page::status(page::Status::Busy, "Preparing…"))
            .push(page::muted(
                "Importing what Connect found and working out which blocks your node still needs.",
            ));
        (StepState::Current, None)
    } else if let Some(job) = job.filter(|_| running) {
        let current = match job.phase {
            Phase::FetchingForLoad => 0,
            Phase::Downloading => 1,
            Phase::Scanning => 2,
            _ => 3,
        };
        let phases = [
            "Fetch blocks needed to load the wallet",
            "Fetch blocks",
            "Scan for this Vault's payments",
            "Check payments and spends",
        ];
        let list = phases.iter().enumerate().skip(
            // Loading only appears when it ran.
            usize::from(current > 0),
        );
        let list = list.fold(Column::new().spacing(6), |column, (index, label)| {
            column.push(page::status(
                if index < current {
                    page::Status::Ok
                } else if index == current {
                    page::Status::Busy
                } else {
                    page::Status::Off
                },
                *label,
            ))
        });
        body = body
            .push(bar(job.progress()))
            .push(page::muted(format!(
                "{} · {percent:.1}% done",
                blocks_line(job)
            )))
            .push(list);
        if let Some(reason) = &job.fallback {
            body = body.push(page::muted(reason.as_str()));
        }
        if let Some(note) = &job.last_error {
            body = body.push(page::muted(note.as_str()));
        }
        let mut actions = Row::new().spacing(8).align_y(Alignment::Center);
        if !status.cancel_queued {
            actions = actions
                .push(page::plain_button(
                    "Pause",
                    ActionKind::Secondary,
                    Some(NodeSettingsMessage::RecoveryPause),
                ))
                .push(page::plain_button(
                    "Cancel",
                    ActionKind::Secondary,
                    Some(NodeSettingsMessage::RecoveryCancel),
                ));
        }
        body = body.push(actions.push(page::muted(if status.cancel_queued {
            "Cancelling after the current batch…"
        } else {
            "Both wait for the current batch. Progress is saved if Tenshu closes."
        })));
        (StepState::Current, Some(format!("{percent:.0}%")))
    } else if let Some(job) = job.filter(|_| paused) {
        body = body.push(bar(job.progress())).push(page::muted(format!(
            "{} · {percent:.1}% done",
            blocks_line(job)
        )));
        if let Some(reason) = &job.fallback {
            body = body.push(page::muted(reason.as_str()));
        }
        body = body.push(page::note(
            Tone::Warn,
            page::muted(
                job.last_error
                    .as_deref()
                    .unwrap_or("Recovery is paused. Nothing already fetched is lost."),
            ),
            None,
        ));
        if !status.cancel_queued {
            body = body.push(
                Row::new()
                    .spacing(8)
                    .push(page::plain_button(
                        "Resume",
                        ActionKind::Primary,
                        can_start.then_some(NodeSettingsMessage::RecoveryResume),
                    ))
                    .push(page::plain_button(
                        "Cancel recovery",
                        ActionKind::Secondary,
                        Some(NodeSettingsMessage::RecoveryCancel),
                    )),
            );
        }
        (StepState::Stopped, Some(format!("Paused at {percent:.0}%")))
    } else {
        body = body
            .push(page::muted(
                "Your node only keeps recent blocks, so it hasn't seen this Vault's older payments yet. Tenshu brings them over and checks every payment and spend against your node.",
            ))
            .push(
                Row::new()
                    .spacing(12)
                    .push(history_source(
                        "Start from Connect's history",
                        Some("Recommended"),
                        "Imports what Connect found and fetches only the blocks it can't prove. Usually the quickest.",
                        !from_date,
                        false,
                    ))
                    .push(history_source(
                        "Start from a date or block",
                        None,
                        "Scans every block from that point, for payments Connect may have missed. Can take hours.",
                        from_date,
                        true,
                    )),
            );
        if from_date {
            let initial_date = chrono::NaiveDate::parse_from_str(start.value.trim(), "%Y-%m-%d")
                .unwrap_or_else(|_| chrono::Local::now().date_naive());
            body = body
                .push(
                    Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push(
                            Container::new(form::Form::new_trimmed(
                                "Block height or YYYY-MM-DD",
                                start,
                                NodeSettingsMessage::RecoveryStartEdited,
                            ))
                            .max_width(320),
                        )
                        .push(date_picker::calendar_button(
                            date_picker_open,
                            initial_date,
                            NodeSettingsMessage::RecoveryDateOpen,
                            NodeSettingsMessage::RecoveryDateCancel,
                            NodeSettingsMessage::RecoveryDateSelected,
                        )),
                )
                .push(page::muted(
                    "Downloads and scans every block from there. Keep Tenshu open; progress is saved if interrupted.",
                ))
                .push(page::plain_button(
                    "Recover from this date or block",
                    ActionKind::Primary,
                    can_start.then_some(NodeSettingsMessage::RecoveryStart),
                ));
        } else {
            body = body.push(page::plain_button(
                "Import history from Connect",
                ActionKind::Primary,
                can_start.then_some(NodeSettingsMessage::ImportConnectHistory),
            ));
        }
        (StepState::Current, None)
    };
    if let Some(error) = &status.error {
        body = body.push(page::note(Tone::Warn, page::muted(error.as_str()), None));
    }
    let step2 = history_step(
        2,
        state2,
        "Bring this Vault's history to your node",
        note2,
        Some(body.into()),
    );

    let step3_body: Element<'a, NodeSettingsMessage> = if switched {
        page::status(page::Status::Ok, "This Vault now uses your local node")
    } else if switching {
        Column::new()
            .spacing(10)
            .push(page::status(
                page::Status::Busy,
                "Switching to your local node…",
            ))
            .push(page::muted(
                "Tenshu is stopping Connect and loading this Vault on your local node.",
            ))
            .into()
    } else if done {
        Column::new()
            .spacing(10)
            .push(page::muted(
                "Switch this Vault to read its balance and history from your node.",
            ))
            .push(page::plain_button(
                "Switch to local node",
                ActionKind::Primary,
                (switch_step == LocalNodeSwitchStep::Ready)
                    .then_some(NodeSettingsMessage::SwitchToBitcoind),
            ))
            .push_maybe((switch_step == LocalNodeSwitchStep::Waiting).then(|| {
                page::muted("Waiting for the node and wallet history checks before switching.")
            }))
            .into()
    } else {
        Row::new()
            .spacing(10)
            .align_y(Alignment::Center)
            .push(page::plain_button(
                "Switch to local node",
                ActionKind::Secondary,
                None,
            ))
            .push(page::muted("Available after step 2"))
            .into()
    };
    let step3 = history_step(
        3,
        if switched {
            StepState::Done
        } else if done {
            StepState::Current
        } else {
            StepState::Todo
        },
        "Switch to your local node",
        if switched { Some("Done".into()) } else { None },
        Some(step3_body),
    );

    let mut card = Column::new()
        .width(Length::Fill)
        .push(
            Container::new(
                Column::new()
                    .spacing(4)
                    .push(page::heading(if switched { "Vault moved to your local node" } else { "Move this Vault to your local node" }))
                    .push(page::muted(if switched {
                        "All steps complete. Tenshu reads this Vault from your node."
                    } else if switching {
                        "Your blockchain and Vault history are ready. Finishing the switch…"
                    } else {
                        "Bring this Vault's history over, then switch. Connect keeps working until you do."
                    })),
            )
            .padding([20, 24]),
        )
        .push(
            Container::new(Column::new().push(step1).push(step2).push(step3))
                .padding([4, 24]),
        );
    let idle = !status.busy
        && !switching
        && job.is_none_or(|job| matches!(job.phase, Phase::Complete | Phase::Cancelled));
    if idle {
        card = card.push(page::rule()).push(
            Container::new(
                Row::new()
                    .spacing(16)
                    .align_y(Alignment::Center)
                    .push(
                        Container::new(page::muted(if switched {
                            "You can switch back to Connect from this page."
                        } else {
                            "Not ready? Connect keeps working, and you can come back here any time."
                        }))
                        .width(Length::Fill),
                    )
                    .push(page::plain_button(
                        if switched { "Done" } else { "Not now" },
                        ActionKind::Secondary,
                        Some(NodeSettingsMessage::RecoveryDismiss),
                    )),
            )
            .padding([14, 24]),
        );
    }
    Column::new()
        .push(Space::new().height(Length::Fixed(page::GROUP_GAP)))
        .push(
            Container::new(card)
                .width(Length::Fill)
                .style(|t: &theme::Theme| {
                    let shell = &t.colors.shell;
                    iced::widget::container::Style {
                        background: Some(iced::Background::Color(shell.panel)),
                        border: iced::Border {
                            color: shell.line,
                            width: 1.0,
                            radius: 12.0.into(),
                        },
                        ..Default::default()
                    }
                }),
        )
        .into()
}

/// Block retention as a settings group: the current policy, an optional
/// rolling window and the apply action.
pub fn rolling_retention_section<'a>(
    days: &'a form::Value<String>,
    status: &'a crate::app::node_history::Status,
    pruning: Option<crate::app::local_switch::NodePruning>,
    local_tip: Option<u64>,
) -> Element<'a, NodeSettingsMessage> {
    use crate::app::local_switch::NodePruning;
    use crate::app::view::settings::page;
    use coincube_ui::component::tenshu::ActionKind;
    let policy = status.policy.as_ref().filter(|policy| !policy.temporary);
    let current = match pruning {
        Some(NodePruning::Unpruned) => {
            "Pruning is off · all downloaded blocks are retained.".into()
        }
        Some(NodePruning::Pruned { prune_height }) => match local_tip {
            Some(tip) if tip >= prune_height => {
                let count = tip - prune_height + 1;
                let unit = if count == 1 { "block" } else { "blocks" };
                format!("{count} {unit} retained · blocks {prune_height}–{tip}.")
            }
            _ => format!(
                "Blocks retained from height {prune_height} · waiting for the current node height."
            ),
        },
        None => "Waiting for the node to report its pruning depth…".into(),
    };
    let policy_description = policy
        .map(|policy| format!("Current target: {} days of blocks.", policy.days))
        .unwrap_or_else(|| {
            if status
                .policy
                .as_ref()
                .is_some_and(|policy| policy.temporary)
            {
                "Blocks are temporarily retained for wallet history recovery.".into()
            } else {
                "No day-based retention window is set.".into()
            }
        });
    let valid = days
        .value
        .parse::<u32>()
        .is_ok_and(|days| (1..=3650).contains(&days));
    let current_row = page::row("Current pruning depth").description(current);
    let row = page::row("Retention window")
        .description(policy_description)
        .line(
            Container::new(Column::new()
                .spacing(6)
                .push(page::muted("Days to retain"))
                .push(
                    Row::new()
                        .spacing(12)
                        .align_y(Alignment::Center)
                        .push(
                            Container::new(form::Form::new_trimmed(
                                "For example, 180 or 365",
                                days,
                                NodeSettingsMessage::RetentionDaysEdited,
                            ))
                            .max_width(320),
                        )
                        .push(page::plain_button(
                            "Apply",
                            ActionKind::Secondary,
                            (!status.busy && valid).then_some(NodeSettingsMessage::RetentionApply),
                        )),
                ))
                .max_width(400),
        )
        .description(
            "Keeps a rolling window of blocks. Older blocks are downloaded if needed and can use hundreds of GB. To return to automatic pruning, apply a storage target in Node resources.",
        )
        .top();
    page::group(
        "Block retention",
        None,
        page::rows([current_row.into(), row.into()]),
    )
}

/// The "Node resources" panel: tune the managed node's prune target and mempool
/// cap, then apply via a force-restart. Reuses the installer's shared controls so
/// the presets/fields never drift, and adds the honesty copy about what changing
/// each value actually does. `processing` is true while a restart is in flight.
pub fn node_resources_section<'a>(
    prune: &'a form::Value<String>,
    max_mempool: &'a form::Value<String>,
    processing: bool,
) -> Element<'a, Message> {
    use crate::app::view::settings::page;
    use crate::node::bitcoind::{
        estimated_total_disk_gb, MAX_MEMPOOL_DEFAULT_MB, MAX_MEMPOOL_SMALL_MB, PRUNE_COMPACT_MB,
        PRUNE_DEFAULT, PRUNE_MINIMAL_MB,
    };
    let nm = node_message;
    // Current values back the honest disk/memory estimate. A blank or
    // unparseable field falls back to the default so the line stays
    // meaningful; a below-floor value is caught by validation on apply.
    let prune_mb = prune.value.parse::<u32>().unwrap_or(PRUNE_DEFAULT);
    let mem_is_default = max_mempool.value.is_empty();
    let mem_mb = if mem_is_default {
        MAX_MEMPOOL_DEFAULT_MB
    } else {
        max_mempool
            .value
            .parse::<u32>()
            .unwrap_or(MAX_MEMPOOL_DEFAULT_MB)
    };
    let mem_is_small = max_mempool.value.parse::<u32>().ok() == Some(MAX_MEMPOOL_SMALL_MB);
    let prune_msg = |mb: u32| nm(NodeSettingsMessage::NodeResourcePruneEdited(mb.to_string()));
    let mem_msg = |v: String| nm(NodeSettingsMessage::NodeResourceMaxMempoolEdited(v));

    let custom = |value: &'a form::Value<String>,
                  placeholder: &'static str,
                  on: fn(String) -> NodeSettingsMessage| {
        Row::new()
            .spacing(6)
            .align_y(Alignment::Center)
            .push(text("Custom (MB):").size(13))
            .push(
                Container::new(Element::<NodeSettingsMessage>::from(
                    form::Form::new_amount_sats(placeholder, value, on)
                        .size(14)
                        .padding(6),
                ))
                .width(Length::Fixed(120.0)),
            )
    };
    let block = |label: &'static str,
                 description: &'static str,
                 controls: Element<'a, Message>|
     -> Element<'a, Message> {
        Container::new(
            Column::new()
                .spacing(2)
                .width(Length::Fill)
                .push(coincube_ui::component::tenshu::label(label))
                .push(page::muted(description))
                .push(Space::new().height(Length::Fixed(8.0)))
                .push(controls),
        )
        .padding([12, 0])
        .width(Length::Fill)
        .into()
    };

    let prune_controls: Element<'a, Message> = Row::new()
        .spacing(12)
        .align_y(Alignment::Center)
        .push(page::plain_segments([
            (
                "Minimal",
                prune_mb == PRUNE_MINIMAL_MB,
                prune_msg(PRUNE_MINIMAL_MB),
            ),
            (
                "Compact",
                prune_mb == PRUNE_COMPACT_MB,
                prune_msg(PRUNE_COMPACT_MB),
            ),
            (
                "Default",
                prune_mb == PRUNE_DEFAULT,
                prune_msg(PRUNE_DEFAULT),
            ),
        ]))
        .push(
            Element::from(custom(
                prune,
                "15000",
                NodeSettingsMessage::NodeResourcePruneEdited,
            ))
            .map(nm),
        )
        .wrap()
        .into();
    let mem_controls: Element<'a, Message> = Column::new()
        .spacing(6)
        .push(
            Row::new()
                .spacing(12)
                .align_y(Alignment::Center)
                .push(page::plain_segments([
                    (
                        "Small (100 MB)",
                        mem_is_small,
                        mem_msg(MAX_MEMPOOL_SMALL_MB.to_string()),
                    ),
                    ("Default (300 MB)", mem_is_default, mem_msg(String::new())),
                ]))
                .push(
                    Element::from(custom(
                        max_mempool,
                        "300",
                        NodeSettingsMessage::NodeResourceMaxMempoolEdited,
                    ))
                    .map(nm),
                )
                .wrap(),
        )
        .push(coincube_ui::component::tenshu::caption(
            "Leave blank for the default (300 MB).",
        ))
        .into();

    let notes = Column::new()
        .spacing(2)
        .width(Length::Fill)
        .push(
            text(format!(
                "Estimated total: about {} GB of disk, and up to {} MB of memory for the mempool.",
                estimated_total_disk_gb(prune_mb),
                mem_mb,
            ))
            .size(14),
        )
        .push(Space::new().height(Length::Fixed(6.0)))
        .push(page::muted(
            "• Lowering the target prunes more block data now, and that can't be undone.",
        ))
        .push(page::muted(
            "• Raising it keeps more blocks from now on — blocks already pruned don't come back.",
        ))
        .push(page::muted(
            "• Restoring an old wallet may require re-downloading blocks below the prune depth.",
        ));

    let rows = page::rows([
        block(
            "Node data to keep",
            "How much block data your node keeps on disk. The chainstate (~14 GB) is always kept and can't be pruned, so total disk use is always higher than this number.",
            prune_controls,
        ),
        page::rule(),
        block(
            "Mempool memory cap",
            "Memory reserved for unconfirmed transactions. During heavy congestion a small cap can delay when small incoming payments first appear, until they confirm.",
            mem_controls,
        ),
        page::rule(),
        page::row("Computer size")
            .control(page::plain_segments([
                (
                    "Small computer",
                    prune_mb == PRUNE_MINIMAL_MB && mem_is_small,
                    nm(NodeSettingsMessage::NodeResourceSmallComputer),
                ),
                (
                    "Regular computer",
                    prune_mb == PRUNE_DEFAULT && mem_is_default,
                    nm(NodeSettingsMessage::NodeResourceRegularComputer),
                ),
            ]))
            .into(),
        page::row("")
            .line(notes)
            .control(page::button(
                NODE_RESOURCES_APPLY_ID,
                if processing { "Restarting node…" } else { "Restart node to apply" },
                (!processing).then_some(nm(NodeSettingsMessage::NodeResourceApply)),
            ))
            .top()
            .into(),
    ]);
    page::group("Node resources", None, rows)
}

pub fn register_wallet_modal<'a>(
    hws: &'a [HardwareWallet],
    processing: bool,
    chosen_hw: Option<usize>,
    registered: &HashSet<Fingerprint>,
) -> Element<'a, Message> {
    card::simple(
        Column::new()
            .push(
                Column::new()
                    .push(text("Select device:").bold().width(Length::Fill))
                    .spacing(10)
                    .push(
                        hws.iter()
                            .enumerate()
                            .fold(Column::new().spacing(10), |col, (i, hw)| {
                                col.push(hw::hw_list_view_for_registration(
                                    i,
                                    hw,
                                    Some(i) == chosen_hw,
                                    processing,
                                    hw.fingerprint()
                                        .map(|f| registered.contains(&f))
                                        .unwrap_or(false)
                                        || if let HardwareWallet::Supported { registered, .. } = hw
                                        {
                                            registered == &Some(true)
                                        } else {
                                            false
                                        },
                                ))
                            }),
                    )
                    .width(Length::Fill),
            )
            .spacing(20)
            .width(Length::Fill)
            .align_x(Alignment::Center),
    )
    .width(Length::Fixed(500.0))
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        app::{
            cache::NodeNetStats,
            menu::{SettingsOption, VaultSubMenu},
        },
        node::bitcoind::NodeResources,
    };
    use std::{net::SocketAddr, path::PathBuf};

    /// Vault › Connections: the node row with a thousands separator and its
    /// own Node settings button, then the Cube connections link; no Connect
    /// account box and no section that only points at Node.
    #[test]
    fn vault_connections_shows_node_health_and_links_to_cube_connections() {
        use crate::app::view::shared::landing::capture;
        let mut cache = Cache {
            connect_authenticated: true,
            connect_stream_status: crate::app::ConnectionStatus::Connected,
            ..Cache::default()
        };
        cache.daemon_cache.blockheight = 974_600;
        cache.daemon_cache.sync_progress = 1.0;
        let probe = capture::inspect(
            "vault-connections",
            connections(&cache),
            (1440, 900),
            "dark",
            coincube_ui::theme::Theme::dark(),
        );
        for label in [
            "Node backend and sync",
            "Synced to block 974,600.",
            "Node settings",
            "Connect account and this device",
            "Cube connections",
        ] {
            assert!(probe.contains(label), "{}", label);
        }
        for gone in ["Cube service session", "Open Node", "Manage in Connect"] {
            assert!(!probe.contains(gone), "{}", gone);
        }
        let sent = capture::activate(
            connections(&cache),
            iced::advanced::widget::Id::new("tenshu.settings.vault-connections.node"),
            (1440, 900),
            true,
        );
        assert!(sent.iter().any(|m| matches!(
            m,
            Message::Menu(Menu::Vault(VaultSubMenu::Settings(Some(
                crate::app::menu::SettingsOption::Node
            ))))
        )));
    }

    fn value(text: &str, valid: bool) -> form::Value<String> {
        form::Value {
            value: text.to_string(),
            valid,
            warning: None,
        }
    }

    fn menu() -> Menu {
        Menu::Vault(VaultSubMenu::Settings(Some(SettingsOption::Node)))
    }

    fn bitcoind_config(rpc_auth: BitcoindRpcAuth) -> coincubed::config::BitcoindConfig {
        coincubed::config::BitcoindConfig {
            rpc_auth,
            addr: "127.0.0.1:8332".parse::<SocketAddr>().unwrap(),
        }
    }

    fn electrum_config() -> coincubed::config::ElectrumConfig {
        coincubed::config::ElectrumConfig {
            addr: "ssl://electrum.example:50002".to_string(),
            validate_domain: true,
        }
    }

    fn cookie_auth_values(valid: bool) -> RpcAuthValues {
        RpcAuthValues {
            cookie_path: value("/tmp/bitcoin/.cookie", valid),
            user: value("", true),
            password: value("", true),
        }
    }

    fn userpass_auth_values(valid: bool) -> RpcAuthValues {
        RpcAuthValues {
            cookie_path: value("", true),
            user: value("rpcuser", valid),
            password: value("rpcpassword", valid),
        }
    }

    #[test]
    fn pure_formatting_helpers_cover_thresholds_and_predicates() {
        assert_eq!(human_bytes(42), "42 B");
        assert_eq!(human_bytes(2 * 1024), "2 KB");
        assert_eq!(human_bytes(45 * 1024 * 1024), "45 MB");
        assert_eq!(human_bytes(1024 * 1024 * 1024), "1.0 GB");

        assert_eq!(expire_message_units(0), Vec::<String>::new());
        assert_eq!(expire_message_units(6), vec!["1h"]);
        assert_eq!(expire_message_units(144), vec!["1d"]);
        assert_eq!(expire_message_units(52_596), vec!["1y"]);

        assert!(is_ok_and(&Ok::<_, ()>(7), |v| *v > 3));
        assert!(!is_ok_and(&Ok::<_, ()>(2), |v| *v > 3));
        assert!(!is_ok_and(&Err::<u32, _>("nope"), |v| *v > 3));
    }

    #[test]
    fn dashboard_wrapped_settings_sections_build() {
        let menu = menu();
        let cache = Cache::default();
        let valid_email = value("friend@example.com", true);
        let invalid_email = value("not an email", false);

        let _ = link("https://example.com/docs", "Docs");
        let _ = list(&menu, &cache, false);
        let _ = bitcoind_settings(&menu, &cache, vec![text("child").into()]);
        let _ = import_export(&menu, &cache, true);
        let _ = import_export(&menu, &cache, false);
        let _ = remote_backend_section(&menu, &cache, &valid_email, false, true);
        let _ = remote_backend_section(&menu, &cache, &invalid_email, true, false);
    }

    #[test]
    fn bitcoind_and_electrum_cards_build_for_configured_and_editing_states() {
        let cookie = BitcoindRpcAuth::CookieFile(PathBuf::from("/tmp/bitcoin/.cookie"));
        let userpass = BitcoindRpcAuth::UserPass("rpcuser".to_string(), "secret".to_string());
        let socket = value("127.0.0.1:8332", true);
        let invalid_socket = value("bad socket", false);
        let cookie_values = cookie_auth_values(true);
        let invalid_userpass_values = userpass_auth_values(false);

        let _ = bitcoind(
            true,
            Network::Bitcoin,
            &bitcoind_config(cookie),
            840_000,
            Some(true),
            true,
            Some(NodeFlavor::Knots),
            Some("/Satoshi:29.3.0/Knots:20260507/"),
        );
        let _ = bitcoind(
            true,
            Network::Testnet,
            &bitcoind_config(userpass),
            0,
            Some(false),
            false,
            None,
            // Version unknown (no poll yet) — the card must simply omit it.
            None,
        );
        let _ = bitcoind_edit(
            true,
            Network::Bitcoin,
            840_000,
            &socket,
            &cookie_values,
            &RpcAuthType::CookieFile,
            false,
        );
        let _ = bitcoind_edit(
            false,
            Network::Signet,
            0,
            &invalid_socket,
            &invalid_userpass_values,
            &RpcAuthType::UserPass,
            true,
        );

        let electrum_cfg = electrum_config();
        let electrum_addr = value("ssl://electrum.example:50002", true);
        let invalid_electrum_addr = value("", false);
        let _ = electrum_edit(true, Network::Bitcoin, 840_000, &electrum_addr, false, true);
        let _ = electrum_edit(
            false,
            Network::Regtest,
            0,
            &invalid_electrum_addr,
            true,
            false,
        );
        let _ = electrum(
            true,
            Network::Bitcoin,
            &electrum_cfg,
            840_000,
            Some(true),
            true,
        );
        let _ = electrum(false, Network::Bitcoin, &electrum_cfg, 0, None, false);
        let _ = is_running_label::<SettingsEditMessage>(Some(true));
        let _ = is_running_label::<SettingsEditMessage>(Some(false));
        let _ = is_running_label::<SettingsEditMessage>(None);
    }

    #[test]
    fn rescan_view_builds_progress_validation_and_processing_states() {
        let good_year = value("2024", true);
        let good_month = value("7", true);
        let good_day = value("20", true);
        let bad_year = value("abcd", false);

        let _ = rescan(
            &good_year,
            &good_month,
            &good_day,
            Some(0.42),
            false,
            false,
            true,
            false,
            false,
            false,
            false,
        );
        let _ = rescan(
            &good_year,
            &good_month,
            &good_day,
            None,
            true,
            false,
            true,
            false,
            false,
            false,
            false,
        );
        let _ = rescan(
            &bad_year,
            &good_month,
            &good_day,
            None,
            false,
            true,
            false,
            true,
            true,
            true,
            true,
        );
    }

    #[test]
    fn node_backend_status_builds_switch_sync_warning_and_setup_states() {
        let stats = NodeNetStats {
            connections_in: 2,
            connections_out: 8,
            upload_used: 45 * 1024 * 1024,
            upload_target: 1024 * 1024 * 1024,
            onion_address: Some("abc123.onion".to_string()),
            subversion: Some("/Satoshi:29.3.0/Knots:20260507/".to_string()),
        };

        let _ = node_backend_status(
            "COINCUBE | Connect",
            icon::network_icon(),
            Some(0.91),
            Some((850_000, 900_000)),
            Some(true),
            Some("/Satoshi:29.3.0/Knots:20260507/"),
            Some("UpdateTip: headers progress"),
            true,
            true,
            true,
            false,
            true,
            true,
            false,
            false,
            Some("Heads up".to_string()),
        );
        let _ = node_backend_status(
            "Local node",
            icon::bitcoin_icon(),
            Some(0.995),
            Some((900_000, 900_000)),
            Some(false),
            // Flavour unknown: the copy must fall back rather than name a build.
            None,
            None,
            false,
            false,
            true,
            false,
            false,
            true,
            false,
            true,
            None,
        );
        let _ = inbound_tor_section(false, true, true, false, false, false, None);
        let _ = inbound_tor_section(true, true, true, true, true, true, Some(&stats));
        let _ = inbound_tor_section(true, false, false, true, false, true, Some(&stats));
    }

    #[tokio::test]
    async fn local_node_sync_status_stays_visible_and_recovery_hides_switch() {
        use iced::advanced::{
            layout,
            renderer::Headless,
            widget::{Id, Operation, Tree},
            Layout,
        };
        #[derive(Default)]
        struct Labels(Vec<String>);
        impl Operation for Labels {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn text(&mut self, _: Option<&Id>, _: iced::Rectangle, text: &str) {
                self.0.push(text.to_owned());
            }
        }
        let renderer = <iced::Renderer as Headless>::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        )
        .await
        .expect("software renderer");
        for (progress, ibd, switching, pending, recovery, heights, expected) in [
            (
                Some(0.975),
                Some(true),
                false,
                true,
                false,
                Some((890000, 900000)),
                "Syncing 97.5%",
            ),
            (
                Some(0.999),
                Some(false),
                true,
                true,
                false,
                Some((900000, 900000)),
                "Blockchain sync complete",
            ),
            (
                Some(0.975),
                Some(true),
                false,
                false,
                false,
                Some((890000, 900000)),
                "Syncing 97.5%",
            ),
            (
                Some(0.999),
                Some(false),
                false,
                false,
                false,
                Some((899990, 900000)),
                "Catching up 99.9%",
            ),
            // The screenshot's last 143 blocks must never display 100%,
            // even if Core's estimate itself is already 1.0.
            (
                Some(1.0),
                Some(true),
                false,
                true,
                false,
                Some((974434, 974577)),
                "Syncing 99.9%",
            ),
            (
                Some(0.999999),
                Some(false),
                false,
                false,
                false,
                Some((899999, 900000)),
                "Catching up 99.9%",
            ),
            // Equal heights alone do not complete initial block download.
            (
                Some(1.0),
                Some(true),
                false,
                true,
                false,
                Some((900000, 900000)),
                "Syncing 99.9%",
            ),
            // Completion must be confirmed, including the known heights.
            (
                Some(1.0),
                Some(false),
                false,
                true,
                false,
                None,
                "Syncing 99.9%",
            ),
            (
                Some(1.0),
                None,
                false,
                true,
                false,
                Some((900000, 900000)),
                "Syncing 99.9%",
            ),
            (
                Some(1.0),
                Some(false),
                false,
                true,
                false,
                Some((900000, 900000)),
                "Blockchain sync complete",
            ),
            (
                None,
                None,
                true,
                true,
                false,
                None,
                "Waiting for the local node to report sync progress…",
            ),
            (
                Some(0.999),
                Some(false),
                false,
                true,
                true,
                Some((900000, 900000)),
                "Blockchain sync complete",
            ),
        ] {
            let mut element = node_backend_status(
                if pending {
                    "COINCUBE | Connect"
                } else {
                    "Local Node"
                },
                icon::bitcoin_icon(),
                progress,
                heights,
                ibd,
                None,
                Some("Loading block index…"),
                true,
                false,
                pending,
                recovery,
                false,
                true,
                false,
                switching,
                None,
            );
            let mut tree = Tree::new(element.as_widget());
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(iced::Size::ZERO, iced::Size::new(1200.0, 1600.0)),
            );
            let mut labels = Labels::default();
            element
                .as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
            assert!(
                labels.0.iter().any(|text| text == expected),
                "missing {expected}: {:?}",
                labels.0
            );
            // An open node log shows whether or not the node reported progress.
            assert!(labels.0.iter().any(|text| text == "Loading block index…"));
            assert!(
                !labels.0.iter().any(|text| text.contains("100.0%")),
                "ongoing sync must never round to 100%: {:?}",
                labels.0
            );
            if let Some((blocks, headers)) = heights {
                let expected_blocks = format!(
                    "Verified blocks: {blocks} / {headers} known headers · {} blocks remaining",
                    headers - blocks
                );
                assert!(labels.0.iter().any(|text| text == &expected_blocks));
            }
            if expected != "Blockchain sync complete" {
                assert!(!labels
                    .0
                    .iter()
                    .any(|text| text == "Blockchain sync complete"));
            }
            if switching {
                assert!(labels
                    .0
                    .iter()
                    .any(|text| text == "Applying your node change…"));
            }
            if !pending {
                assert!(!labels
                    .0
                    .iter()
                    .any(|text| text.contains("automatically switch")));
            }
            assert_eq!(
                labels.0.iter().any(|text| text == "Switch to local node"),
                pending && ibd != Some(true) && !recovery
            );
            if recovery {
                assert!(labels.0.iter().any(|text| {
                    text.contains("Bring this Vault's history over below, then switch")
                }));
            }
        }
    }

    #[tokio::test]
    async fn node_retention_reports_actual_depth_separately_from_target() {
        use crate::app::{local_switch::NodePruning, node_history::Status};
        use iced::advanced::{
            layout,
            renderer::Headless,
            widget::{Id, Operation, Tree},
            Layout,
        };

        #[derive(Default)]
        struct Labels(Vec<(String, iced::Rectangle)>);
        impl Operation for Labels {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn text(&mut self, _: Option<&Id>, bounds: iced::Rectangle, text: &str) {
                self.0.push((text.to_owned(), bounds));
            }
        }
        for bytes in [
            coincube_ui::font::REGULAR_BYTES,
            coincube_ui::font::MEDIUM_BYTES,
            coincube_ui::font::BOLD_BYTES,
            coincube_ui::font::BOOTSTRAP_ICONS_BYTE,
        ] {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(std::borrow::Cow::Borrowed(bytes));
        }
        let mut renderer = <iced::Renderer as Headless>::new(
            coincube_ui::font::REGULAR,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        )
        .await
        .expect("software renderer");
        let days = value("", true);
        for (scenario, pruning, tip, target, expected) in [
            (
                "pruned",
                Some(NodePruning::Pruned {
                    prune_height: 970000,
                }),
                Some(974597),
                None,
                "4598 blocks retained · blocks 970000–974597.",
            ),
            (
                "rolling",
                Some(NodePruning::Pruned {
                    prune_height: 970000,
                }),
                Some(974597),
                Some(180),
                "4598 blocks retained · blocks 970000–974597.",
            ),
            (
                "single-block",
                Some(NodePruning::Pruned {
                    prune_height: 974597,
                }),
                Some(974597),
                None,
                "1 block retained · blocks 974597–974597.",
            ),
            (
                "unpruned",
                Some(NodePruning::Unpruned),
                Some(974597),
                None,
                "Pruning is off · all downloaded blocks are retained.",
            ),
            (
                "unknown",
                None,
                None,
                None,
                "Waiting for the node to report its pruning depth…",
            ),
            (
                "unknown-tip",
                Some(NodePruning::Pruned {
                    prune_height: 970000,
                }),
                None,
                None,
                "Blocks retained from height 970000 · waiting for the current node height.",
            ),
        ] {
            let status = Status {
                policy: target.map(|days| crate::node::retention::Policy {
                    version: 1,
                    days,
                    previous_size_mb: 15000,
                    temporary: false,
                    keep_from: None,
                }),
                ..Status::default()
            };
            for (appearance, theme) in [
                ("dark", theme::Theme::dark()),
                ("light", theme::Theme::light()),
            ] {
                for width in [760, 1200] {
                    let mut element: Element<NodeSettingsMessage> = Container::new(
                        Column::new()
                            .push(node_backend_status(
                                "Local Node",
                                icon::bitcoin_icon(),
                                Some(1.0),
                                Some((974597, 974597)),
                                Some(false),
                                Some("/Satoshi:29.0.0/"),
                                Some("Node log"),
                                false,
                                true,
                                false,
                                false,
                                false,
                                false,
                                false,
                                false,
                                None,
                            ))
                            .push(rolling_retention_section(&days, &status, pruning, tip)),
                    )
                    .padding(24)
                    .width(Length::Fill)
                    .into();
                    let size = iced::Size::new(width as f32, 650.0);
                    let mut tree = Tree::new(element.as_widget());
                    let node = element.as_widget_mut().layout(
                        &mut tree,
                        &renderer,
                        &layout::Limits::new(iced::Size::ZERO, size),
                    );
                    let mut labels = Labels::default();
                    element.as_widget_mut().operate(
                        &mut tree,
                        Layout::new(&node),
                        &renderer,
                        &mut labels,
                    );
                    assert!(
                        labels.0.iter().any(|(text, _)| text == expected),
                        "{}: {}",
                        scenario,
                        expected
                    );
                    assert!(!labels
                        .0
                        .iter()
                        .any(|(text, _)| text.contains("has finished initial")));
                    assert!(!labels.0.iter().any(|(text, _)| text == "Keep more history"));
                    assert_eq!(
                        labels
                            .0
                            .iter()
                            .any(|(text, _)| text == "Current target: 180 days of blocks."),
                        target.is_some()
                    );
                    let bounds =
                        |label: &str| labels.0.iter().find(|(text, _)| text == label).unwrap().1;
                    let title = bounds("Local node · Bitcoin Core 29.0.0");
                    let complete = bounds("Blockchain sync complete");
                    assert!(complete.x > title.x + title.width);
                    assert!((complete.center_y() - title.center_y()).abs() < 2.0);
                    for (text, bounds) in &labels.0 {
                        assert!(
                            bounds.x >= 0.0 && bounds.x + bounds.width <= size.width + 1.0,
                            "{}/{}: text exceeds width: {}: {:?}",
                            scenario,
                            width,
                            text,
                            bounds
                        );
                    }
                    if matches!(scenario, "pruned" | "rolling") {
                        if let Ok(directory) = std::env::var("NODE_SETTINGS_PREVIEW_DIR") {
                            let viewport = iced::Rectangle::with_size(size);
                            element.as_widget_mut().update(
                                &mut tree,
                                &iced::Event::Window(iced::window::Event::RedrawRequested(
                                    std::time::Instant::now(),
                                )),
                                Layout::new(&node),
                                iced::mouse::Cursor::Unavailable,
                                &renderer,
                                &mut iced::advanced::clipboard::Null,
                                &mut iced::advanced::Shell::new(&mut Vec::new()),
                                &viewport,
                            );
                            iced::advanced::Renderer::reset(&mut renderer, viewport);
                            element.as_widget().draw(
                                &tree,
                                &mut renderer,
                                &theme,
                                &iced::advanced::renderer::Style {
                                    text_color: theme.colors.text.primary,
                                },
                                Layout::new(&node),
                                iced::mouse::Cursor::Unavailable,
                                &viewport,
                            );
                            let pixels = renderer.screenshot(
                                iced::Size::new(width, 650),
                                1.0,
                                theme.colors.general.background,
                            );
                            image::save_buffer(
                                std::path::Path::new(&directory)
                                    .join(format!("node-{scenario}-{appearance}-{width}.png")),
                                &pixels,
                                width,
                                650,
                                image::ColorType::Rgba8,
                            )
                            .expect("save node settings preview");
                        }
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn history_card_keeps_every_step_and_says_why_switch_waits() {
        use crate::app::node_history::Status;
        use crate::node::history::{Binding, Phase, Recovery};
        use coincube_core::miniscript::bitcoin::{hashes::Hash, BlockHash};
        use iced::advanced::{
            layout,
            renderer::Headless,
            widget::{Id, Operation, Tree},
            Layout,
        };
        #[derive(Default)]
        struct Labels(Vec<String>);
        impl Operation for Labels {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn text(&mut self, _: Option<&Id>, _: iced::Rectangle, text: &str) {
                self.0.push(text.to_owned());
            }
        }
        let renderer = <iced::Renderer as Headless>::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        )
        .await
        .expect("software renderer");
        let job = |phase: Phase| {
            let binding = Binding {
                genesis: BlockHash::all_zeros(),
                node_instance: "fixture".into(),
                endpoint: "fixture".into(),
                wallet: "fixture".into(),
                descriptor: "fixture".into(),
            };
            let mut job = Recovery::new(binding, 100, 200, BlockHash::all_zeros()).unwrap();
            job.phase = phase;
            job
        };
        let start = value("", false);
        let mut paused = job(Phase::Paused);
        paused.last_error = Some("Cannot reach local node for getblockhash".into());
        let mut complete = job(Phase::Complete);
        complete.next = 201;
        let cases: Vec<(Status, bool, &[&str], &[&str])> = vec![
            (
                Status::default(),
                false,
                &[
                    "Sync the blockchain",
                    "Import history from Connect",
                    "Available after step 2",
                    "Not now",
                ],
                &["Pause", "Resume"],
            ),
            (
                Status::default(),
                true,
                &["Recover from this date or block"],
                &["Import history from Connect"],
            ),
            (
                Status {
                    busy: true,
                    ..Status::default()
                },
                false,
                &["Preparing…", "Available after step 2"],
                &["Import history from Connect", "Not now"],
            ),
            (
                Status {
                    job: Some(job(Phase::Downloading)),
                    ..Status::default()
                },
                false,
                &["Fetch blocks", "Pause", "Cancel", "Available after step 2"],
                &["Resume", "Not now"],
            ),
            (
                Status {
                    job: Some(paused),
                    ..Status::default()
                },
                false,
                &[
                    "Resume",
                    "Cancel recovery",
                    "Cannot reach local node for getblockhash",
                ],
                &["Pause"],
            ),
            (
                Status {
                    job: Some(complete),
                    reconciled: true,
                    ..Status::default()
                },
                false,
                &[
                    "History matches your node",
                    "Switch to local node",
                    "Not now",
                ],
                &["Available after step 2"],
            ),
        ];
        for (status, from_date, present, absent) in cases {
            let mut element = wallet_history_section(
                &start,
                &status,
                from_date,
                false,
                Some(974215),
                if status.reconciled {
                    LocalNodeSwitchStep::Ready
                } else {
                    LocalNodeSwitchStep::Waiting
                },
            );
            let mut tree = Tree::new(element.as_widget());
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(iced::Size::ZERO, iced::Size::new(1200.0, 2400.0)),
            );
            let mut labels = Labels::default();
            element
                .as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
            for label in present {
                assert!(
                    labels.0.iter().any(|text| text == label),
                    "missing {label}: {:?}",
                    labels.0
                );
            }
            for label in absent {
                assert!(
                    !labels.0.iter().any(|text| text == label),
                    "unexpected {label}: {:?}",
                    labels.0
                );
            }
        }
    }

    #[tokio::test]
    async fn local_node_wizard_shows_switching_then_all_steps_complete() {
        use iced::advanced::{
            layout,
            renderer::Headless,
            widget::{Id, Operation, Tree},
            Layout,
        };
        #[derive(Default)]
        struct Labels(Vec<String>, Option<iced::Rectangle>);
        impl Operation for Labels {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn text(&mut self, _: Option<&Id>, bounds: iced::Rectangle, text: &str) {
                self.0.push(text.to_owned());
                if text == "Done" {
                    self.1 = Some(bounds);
                }
            }
        }
        for bytes in coincube_ui::font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let start = value("", false);
        let status = crate::app::node_history::Status::default();
        for (theme_name, theme) in [
            ("dark", theme::Theme::dark()),
            ("light", theme::Theme::light()),
        ] {
            for (name, step) in [
                ("ready", LocalNodeSwitchStep::Ready),
                ("switching", LocalNodeSwitchStep::Switching),
                ("complete", LocalNodeSwitchStep::Complete),
            ] {
                let mut renderer = <iced::Renderer as Headless>::new(
                    coincube_ui::font::REGULAR,
                    iced::Pixels(16.0),
                    Some("tiny-skia"),
                )
                .await
                .expect("software renderer");
                // Successful restart clears the RPC/history caches. Completion
                // must remain visible even before the replacement probe answers.
                let height = (step != LocalNodeSwitchStep::Complete).then_some(974594);
                let mut element =
                    wallet_history_section(&start, &status, false, false, height, step);
                let mut tree = Tree::new(element.as_widget());
                let size = iced::Size::new(1000.0, 720.0);
                let node = element.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(iced::Size::ZERO, size),
                );
                let mut labels = Labels::default();
                element.as_widget_mut().operate(
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &mut labels,
                );
                for title in [
                    "Sync the blockchain",
                    "Bring this Vault's history to your node",
                    "Switch to your local node",
                ] {
                    assert!(labels.0.iter().any(|text| text == title));
                }
                assert_eq!(
                    labels.0.iter().any(|text| text == "Switch to local node"),
                    step == LocalNodeSwitchStep::Ready
                );
                assert_eq!(
                    labels
                        .0
                        .iter()
                        .any(|text| text == "Switching to your local node…"),
                    step == LocalNodeSwitchStep::Switching
                );
                if step == LocalNodeSwitchStep::Complete {
                    assert!(labels
                        .0
                        .iter()
                        .any(|text| text == "Vault moved to your local node"));
                    assert_eq!(
                        labels
                            .0
                            .iter()
                            .filter(|text| text.as_str() == "Done")
                            .count(),
                        4,
                        "all three steps plus the Done action: {:?}",
                        labels.0
                    );
                    assert!(!labels
                        .0
                        .iter()
                        .any(|text| text.contains("Connect keeps working")));
                    let mut messages = Vec::new();
                    for event in [
                        iced::mouse::Event::ButtonPressed(iced::mouse::Button::Left),
                        iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left),
                    ] {
                        element.as_widget_mut().update(
                            &mut tree,
                            &iced::Event::Mouse(event),
                            Layout::new(&node),
                            iced::mouse::Cursor::Available(labels.1.expect("Done action").center()),
                            &renderer,
                            &mut iced::advanced::clipboard::Null,
                            &mut iced::advanced::Shell::new(&mut messages),
                            &iced::Rectangle::with_size(size),
                        );
                    }
                    assert!(
                        matches!(messages.as_slice(), [NodeSettingsMessage::RecoveryDismiss]),
                        "Done dismisses the completed checklist"
                    );
                }
                if step == LocalNodeSwitchStep::Switching {
                    assert!(!labels.0.iter().any(|text| text == "Not now"
                        || text == "All steps complete. Tenshu reads this Vault from your node."));
                }
                if let Ok(directory) = std::env::var("LOCAL_NODE_WIZARD_CAPTURE_DIR") {
                    std::fs::create_dir_all(&directory).unwrap();
                    let mut messages = Vec::new();
                    element.as_widget_mut().update(
                        &mut tree,
                        &iced::Event::Window(iced::window::Event::RedrawRequested(
                            std::time::Instant::now(),
                        )),
                        Layout::new(&node),
                        iced::mouse::Cursor::Unavailable,
                        &renderer,
                        &mut iced::advanced::clipboard::Null,
                        &mut iced::advanced::Shell::new(&mut messages),
                        &iced::Rectangle::with_size(size),
                    );
                    element.as_widget().draw(
                        &tree,
                        &mut renderer,
                        &theme,
                        &iced::advanced::renderer::Style {
                            text_color: theme.colors.text.primary,
                        },
                        Layout::new(&node),
                        iced::mouse::Cursor::Unavailable,
                        &iced::Rectangle::with_size(size),
                    );
                    let pixels = renderer.screenshot(
                        iced::Size::new(1000, 720),
                        1.0,
                        theme.colors.general.background,
                    );
                    image::save_buffer(
                        std::path::Path::new(&directory).join(format!("{name}-{theme_name}.png")),
                        &pixels,
                        1000,
                        720,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                }
            }
        }
    }

    // The copy names what actually differs — relay policy — and, in both
    // directions, says the chain is untouched. It previously had to warn that a
    // switch could move the node onto a different chain, which was true only while
    // one of the two builds enforced consensus rules the other did not. Repeating
    // that warning now would scare users off a choice that no longer has stakes.
    #[test]
    fn flavor_switch_copy_is_policy_only() {
        let to_core = flavor_switch_chain_note(NodeFlavor::Core);
        let to_knots = flavor_switch_chain_note(NodeFlavor::Knots);
        assert_ne!(to_core, to_knots);
        for (target, note) in [("Core", to_core), ("Knots", to_knots)] {
            assert!(
                note.contains("unaffected"),
                "switching to {} must say the chain and balances are unaffected",
                target
            );
            for dead in ["BIP-110", "RDTS", "different chain", "re-check"] {
                assert!(
                    !note.contains(dead),
                    "switching to {} still mentions {:?}",
                    target,
                    dead
                );
            }
        }
        assert!(
            to_knots.contains("stricter"),
            "the Knots copy must say what the choice actually changes"
        );
    }

    // The grep gate from the sunset plan, enforced where the strings live: no copy
    // the node settings page can render names the dead soft fork.
    #[test]
    fn node_settings_copy_never_names_the_soft_fork() {
        for note in [
            flavor_switch_chain_note(NodeFlavor::Core),
            flavor_switch_chain_note(NodeFlavor::Knots),
        ] {
            assert!(!note.contains("RDTS") && !note.contains("BIP-110"));
        }
    }

    #[test]
    fn local_switch_confirmation_and_pruned_notice_build() {
        use crate::app::local_switch::{PrunedHistory, RunningScan};
        let _ = switch_discards_scan_confirm(RunningScan::WalletSync);
        let _ = switch_discards_scan_confirm(RunningScan::Rescan);
        let _: Element<'_, NodeSettingsMessage> =
            pruned_node_notice(PrunedHistory::CoinsBelowPrune {
                prune_height: 969_938,
                earliest: 958_601,
            });
        let _: Element<'_, NodeSettingsMessage> =
            pruned_node_notice(PrunedHistory::HistoryUnknown {
                prune_height: 969_938,
            });
    }

    #[test]
    fn node_setup_and_resource_panels_build_for_all_stages() {
        let addr = value("127.0.0.1:8332", true);
        let invalid_addr = value("bad", false);
        let cookie_values = cookie_auth_values(false);
        let userpass_values = userpass_auth_values(false);
        let prune = value("15000", true);
        let max_mempool = value("", true);

        let _ = pending_node_setup_panel(&addr, &cookie_values, &RpcAuthType::CookieFile, false);
        let _ = pending_node_setup_panel(
            &invalid_addr,
            &userpass_values,
            &RpcAuthType::UserPass,
            true,
        );
        let _ = node_setup_mode_picker_panel();
        let _ = internal_node_setup_panel(NodeFlavor::Knots, true, false, false, None, 72.0);
        let _ = internal_node_setup_panel(NodeFlavor::Core, false, true, false, None, 0.0);
        let _ = internal_node_setup_panel(NodeFlavor::Knots, false, false, true, None, 100.0);
        let _ = internal_node_setup_panel(
            NodeFlavor::Core,
            false,
            false,
            false,
            Some("download failed"),
            0.0,
        );
        let _ = flavor_switch_confirm(NodeFlavor::Knots);
        let _ = node_resources_section(&prune, &max_mempool, false);
        let _ = node_resources_section(&prune, &max_mempool, true);

        assert_eq!(NodeResources::small_computer().max_mempool_mb, Some(100));
        assert_eq!(NodeResources::regular_computer().max_mempool_mb, None);
    }
}
