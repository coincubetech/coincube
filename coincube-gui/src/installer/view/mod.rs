pub mod editor;

use async_hwi::utils::extract_keys_and_template;
use iced::widget::{checkbox, radio, scrollable, tooltip, Button, Space, TextInput};
use iced::{
    alignment,
    widget::{progress_bar, tooltip as iced_tooltip},
    Alignment, Length,
};

use coincube_core::miniscript::bitcoin::bip32::ChildNumber;
use coincube_ui::component::text::{self, p2_regular};
use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;
use std::{collections::HashSet, str::FromStr};

use coincube_core::{
    descriptors::{CoincubeDescriptor, CoincubePolicy},
    miniscript::bitcoin::{self, bip32::Fingerprint},
};
use coincube_ui::{
    component::{
        button, card, collapse, form, hw,
        text::{h2, h3, h4_bold, p1_bold, p1_regular, text, Text},
    },
    icon, theme,
    widget::{action::Action, *},
};

use crate::node::electrum::validate_domain_checkbox;
use crate::{
    app::settings,
    help,
    hw::{is_compatible_with_tapminiscript, HardwareWallet, UnsupportedReason},
    installer::{
        descriptor::{PathSequence, PathWarning},
        message::{self, CoincubeConnectMsg, DefineBitcoind, DefineEsplora, DefineNode, Message},
        prompt,
        step::{DownloadState, InstallState},
        view::editor::format_sequence_duration,
        Error,
    },
    node::{
        bitcoind::{ConfigField, RpcAuthType, RpcAuthValues, StartInternalBitcoindError},
        electrum, esplora, NodeType,
    },
};

pub fn import_wallet_or_descriptor<'a>(
    progress: (usize, usize),
    email: Option<&'a str>,
    invitation: &'a form::Value<String>,
    invitation_wallet: Option<&'a str>,
    imported_descriptor: &'a form::Value<String>,
    error: Option<&'a String>,
    wallets: Vec<(&'a String, Option<&'a String>)>,
) -> Element<'a, Message> {
    let mut col_wallets = Column::new()
        .spacing(20)
        .push(h4_bold("Wallets on this Connect account"))
        .push(p2_regular(
            "Choose a wallet already available to this Cube.",
        ));
    let no_wallets = wallets.is_empty();
    for (i, (name, alias)) in wallets.into_iter().enumerate() {
        col_wallets = col_wallets.push(
            Button::new(
                Column::new()
                    .push(alias.map(p1_bold))
                    .push(p1_regular(name))
                    .width(Length::Fill),
            )
            .style(theme::button::secondary)
            .padding(10)
            .on_press(Message::Select(i)),
        );
    }
    let card_wallets: Element<'a, Message> = if no_wallets {
        p2_regular("No previously used wallets are available on this account.").into()
    } else {
        card::simple(col_wallets).into()
    };

    let col_invitation_token = collapse::Collapse::new(
        || {
            Button::new(
                Column::new()
                    .spacing(5)
                    .push(h4_bold("Load a shared wallet").style(theme::text::primary))
                    .push(
                        text("Use an invitation token from a shared wallet")
                            .style(theme::text::secondary),
                    ),
            )
            .padding(15)
            .width(Length::Fill)
            .style(theme::button::transparent_border)
        },
        || {
            Button::new(
                Column::new()
                    .spacing(5)
                    .push(h4_bold("Load a shared wallet").style(theme::text::primary))
                    .push(
                        text("Paste the invitation token you received by email")
                            .style(theme::text::secondary),
                    ),
            )
            .padding(15)
            .width(Length::Fill)
            .style(theme::button::transparent_border)
        },
        move || {
            if let Some(wallet) = invitation_wallet {
                Element::<'a, Message>::from(
                    Column::new()
                        .push(Space::new().height(0))
                        .push(
                            Row::new()
                                .spacing(5)
                                .push(Space::new().width(15))
                                .push(text("Accept invitation for wallet:"))
                                .push(text(wallet).bold()),
                        )
                        .push(
                            Row::new()
                                .push(Space::new().width(Length::Fill))
                                .push(
                                    button::secondary(None, "Accept")
                                        .width(Length::Fixed(200.0))
                                        .on_press(Message::ImportRemoteWallet(
                                            message::ImportRemoteWallet::AcceptInvitation,
                                        )),
                                )
                                .push(Space::new().width(Length::Fill)),
                        )
                        .push(Space::new().width(5))
                        .spacing(20),
                )
            } else {
                Element::<'a, Message>::from(
                    Container::new(
                        Column::new()
                            .push(Space::new().height(0))
                            .push(
                                Column::new()
                                    .push(text("Paste invitation:").bold())
                                    .push(
                                        form::Form::new_trimmed("Invitation", invitation, |msg| {
                                            Message::ImportRemoteWallet(
                                                message::ImportRemoteWallet::ImportInvitationToken(
                                                    msg,
                                                ),
                                            )
                                        })
                                        .warning("Invitation token is invalid or expired")
                                        .size(text::P1_SIZE)
                                        .padding(10),
                                    )
                                    .spacing(10),
                            )
                            .push(
                                Row::new().push(Space::new().width(Length::Fill)).push(
                                    button::secondary(None, "Next")
                                        .width(Length::Fixed(200.0))
                                        .on_press_maybe(if !invitation.value.is_empty() {
                                            Some(Message::ImportRemoteWallet(
                                                message::ImportRemoteWallet::FetchInvitation,
                                            ))
                                        } else {
                                            None
                                        }),
                                ),
                            )
                            .spacing(20),
                    )
                    .padding(15),
                )
            }
        },
    );

    let col_descriptor = collapse::Collapse::new(
        || {
            Button::new(
                Column::new()
                    .spacing(5)
                    .push(h4_bold("Load a wallet from descriptor").style(theme::text::primary))
                    .push(
                        text("Import its public descriptor; signing keys are not imported here")
                            .style(theme::text::secondary),
                    ),
            )
            .padding(15)
            .width(Length::Fill)
            .style(theme::button::transparent_border)
        },
        || {
            Button::new(
                Column::new()
                    .spacing(5)
                    .push(h4_bold("Load a wallet from descriptor").style(theme::text::primary))
                    .push(
                        text("Import its public descriptor; signing keys are not imported here")
                            .style(theme::text::secondary),
                    ),
            )
            .padding(15)
            .width(Length::Fill)
            .style(theme::button::transparent_border)
        },
        move || {
            Element::<'a, Message>::from(
                Container::new(
                    Column::new()
                        .push(Space::new().height(0))
                        .push(
                            Column::new()
                                .push(text("Descriptor:").bold())
                                .push(
                                    form::Form::new_trimmed(
                                        "Descriptor",
                                        imported_descriptor,
                                        |msg| {
                                            Message::ImportRemoteWallet(
                                                message::ImportRemoteWallet::ImportDescriptor(msg),
                                            )
                                        },
                                    )
                                    .warning(
                                        "Either descriptor is invalid or incompatible with network",
                                    )
                                    .size(text::P1_SIZE)
                                    .padding(10),
                                )
                                .push(text("or").bold())
                                .push(button::primary(None, "Import descriptor").on_press(
                                    Message::ImportRemoteWallet(
                                        message::ImportRemoteWallet::ImportDescriptorFromFile,
                                    ),
                                ))
                                .spacing(10),
                        )
                        .push(
                            Row::new().push(Space::new().width(Length::Fill)).push(
                                button::secondary(None, "Next")
                                    .width(Length::Fixed(200.0))
                                    .on_press_maybe(
                                        if imported_descriptor.value.is_empty()
                                            || !imported_descriptor.valid
                                        {
                                            None
                                        } else {
                                            Some(Message::ImportRemoteWallet(
                                                message::ImportRemoteWallet::ConfirmDescriptor,
                                            ))
                                        },
                                    ),
                            ),
                        )
                        .spacing(20),
                )
                .padding(15),
            )
        },
    );

    layout(
        progress,
        email,
        "Add an existing wallet",
        Column::new()
            .spacing(24)
            .push(h4_bold("Choose a wallet source"))
            .push(p2_regular(
                "Use a wallet on this account, a shared invitation, or a public descriptor. To restore a Cube from its Recovery Kit, use the Cube restore path.",
            ))
            .push(error.map(|e| card::error("Something wrong happened", e.to_string())))
            .push(card_wallets)
            .push(card::simple(col_invitation_token).padding(0))
            .push(card::simple(col_descriptor).padding(0))
            .push(Space::new().height(10)),
        true,
        Some(Message::Previous),
    )
}

pub fn import_descriptor<'a>(
    progress: (usize, usize),
    email: Option<&'a str>,
    imported_descriptor: &form::Value<String>,
    imported_backup: bool,
    wrong_network: bool,
    error: Option<&String>,
) -> Element<'a, Message> {
    let valid = !imported_descriptor.value.is_empty() && imported_descriptor.valid;

    let col_descriptor = Column::new()
        .push(text("Descriptor:").bold())
        .push(Space::new().height(10))
        .push(
            form::Form::new_trimmed("Descriptor", imported_descriptor, |msg| {
                Message::DefineDescriptor(message::DefineDescriptor::ImportDescriptor(msg))
            })
            .warning(if wrong_network {
                "The descriptor is for another network"
            } else {
                "Failed to read the descriptor"
            })
            .size(text::P1_SIZE)
            .padding(10),
        );

    let descriptor = if imported_backup {
        None
    } else {
        Some(col_descriptor)
    };

    let or = if !valid && !imported_backup {
        Some(
            Row::new()
                .push(text("or").bold())
                .push(Space::new().width(Length::Fill)),
        )
    } else {
        None
    };

    let import_backup = if !valid && !imported_backup {
        Some(
            Row::new()
                .push(button::primary(None, "Import backup").on_press(Message::ImportBackup))
                .push(Space::new().width(Length::Fill)),
        )
    } else {
        None
    };

    let backup_imported = if imported_backup {
        Some(
            Row::new()
                .push(text("Backup successfully imported!").bold())
                .push(Space::new().width(Length::Fill)),
        )
    } else {
        None
    };

    layout(
        progress,
        email,
        "Import a public wallet descriptor",
        Column::new()
            .push(p2_regular(
                "This step reads a public descriptor or backup. It does not add signing keys. You can enter the wallet's recovery words in the next step or skip that step if its keys are held elsewhere.",
            ))
            .push(
                Column::new()
                    .spacing(20)
                    .push(import_backup)
                    .push(backup_imported)
                    .push(or)
                    .push(descriptor)
                    .push(text(
                        "If you are using a Bitcoin Core node, \
                you will need to perform a rescan of \
                the blockchain after creating the wallet \
                in order to see your coins and past \
                transactions. This can be done in \
                Settings > Node.",
                    )),
            )
            .push(
                if imported_descriptor.value.is_empty() || !imported_descriptor.valid {
                    button::secondary(None, "Next").width(Length::Fixed(200.0))
                } else {
                    button::secondary(None, "Next")
                        .width(Length::Fixed(200.0))
                        .on_press(Message::Next)
                },
            )
            .push(error.map(|e| card::error("Invalid descriptor", e.to_string())))
            .spacing(50),
        true,
        Some(Message::Previous),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn register_descriptor<'a>(
    progress: (usize, usize),
    email: Option<&'a str>,
    descriptor: &'a CoincubeDescriptor,
    hws: &'a [HardwareWallet],
    registered: &HashSet<bitcoin::bip32::Fingerprint>,
    error: Option<&Error>,
    processing: bool,
    chosen_hw: Option<usize>,
    done: bool,
    created_desc: bool,
) -> Element<'a, Message> {
    let descriptor_str = descriptor.to_string();
    let displayed_descriptor =
        if let Ok((template, keys)) = extract_keys_and_template::<String>(&descriptor_str) {
            let mut col = Column::new()
                .push(
                    card::simple(
                        Column::new()
                            .push(text("Descriptor template:").small().bold())
                            .push(
                                scrollable(
                                    Column::new()
                                        .push(text(template).small())
                                        .push(Space::new().height(Length::Fixed(5.0))),
                                )
                                .direction(
                                    scrollable::Direction::Horizontal(
                                        scrollable::Scrollbar::new().width(5).scroller_width(5),
                                    ),
                                ),
                            )
                            .spacing(10),
                    )
                    .width(Length::Fill),
                )
                .push(Space::new().height(5));

            for (index, key) in keys.into_iter().enumerate() {
                col = col
                    .push(
                        card::simple(
                            Column::new()
                                .push(text(format!("Key @{}:", index)).small().bold())
                                .push(
                                    scrollable(
                                        Column::new()
                                            .push(text(key.to_owned()).small())
                                            .push(Space::new().height(Length::Fixed(5.0))),
                                    )
                                    .direction(
                                        scrollable::Direction::Horizontal(
                                            scrollable::Scrollbar::new().width(5).scroller_width(5),
                                        ),
                                    ),
                                )
                                .spacing(10),
                        )
                        .width(Length::Fill),
                    )
                    .push(Space::new().height(5));
            }

            col
        } else {
            Column::new().push(card::simple(
                Column::new()
                    .push(text("The descriptor:").small().bold())
                    .push(
                        scrollable(
                            Column::new()
                                .push(text(descriptor_str.to_owned()).small())
                                .push(Space::new().height(Length::Fixed(5.0))),
                        )
                        .direction(scrollable::Direction::Horizontal(
                            scrollable::Scrollbar::new().width(5).scroller_width(5),
                        )),
                    )
                    .push(
                        Row::new().push(Column::new().width(Length::Fill)).push(
                            button::secondary(Some(icon::clipboard_icon()), "Copy")
                                .on_press(Message::Clipboard(descriptor_str)),
                        ),
                    )
                    .spacing(10),
            ))
        };
    layout(
        progress,
        email,
        "Register descriptor",
        Column::new()
            .push((!created_desc).then_some(
                text("This step is only necessary if you are using a signing device.").bold(),
            ))
            .push(displayed_descriptor)
            .push(text(prompt::REGISTER_DESCRIPTOR_HELP))
            .push(error.map(|e| card::error("Failed to register descriptor", e.to_string())))
            .push(
                Column::new()
                    .push(
                        Container::new(
                            if created_desc {
                                text("Select hardware wallet to register descriptor on:")
                                    .bold()
                            } else {
                                text("If necessary, please select the signing device to register descriptor on:")
                                    .bold()
                            },
                        )
                        .width(Length::Fill),
                    )
                    .spacing(10)
                    .push(
                        hws.iter()
                            .enumerate()
                            .fold(Column::new().spacing(10), |col, (i, hw)| {
                                col.push(hw_list_view(
                                    i,
                                    hw,
                                    Some(i) == chosen_hw,
                                    processing,
                                    hw.fingerprint()
                                        .map(|fg| registered.contains(&fg))
                                        .unwrap_or(false),
                                    Some(descriptor),
                                    false,
                                    None,
                                    false,
                                ))
                            }),
                    )
                    .width(Length::Fill),
            )
            .push(created_desc.then_some(checkbox(done)
                .label("I have registered the descriptor on my device(s)")
            .on_toggle(Message::UserActionDone)))
            .push(if !created_desc || (done && !processing) {
                button::secondary(None, "Next")
                    .on_press(Message::Next)
                    .width(Length::Fixed(200.0))
            } else {
                button::secondary(None, "Next").width(Length::Fixed(200.0))
            })
            .push(Space::new().height(5))
            .spacing(50),
        true,
        if !processing {
        Some(Message::Previous)
        } else {
            None
        }
    )
}

pub fn backup_descriptor<'a>(
    progress: (usize, usize),
    email: Option<&'a str>,
    descriptor: &'a CoincubeDescriptor,
    keys: &'a HashMap<Fingerprint, settings::KeySetting>,
    error: Option<&Error>,
    done: bool,
) -> Element<'a, Message> {
    let backup_button = if done {
        button::secondary(Some(icon::backup_icon()), "Back Up Descriptor")
            .on_press(Message::BackupDescriptor)
    } else {
        button::primary(Some(icon::backup_icon()), "Back Up Descriptor")
            .on_press(Message::BackupDescriptor)
    };

    layout(
        progress,
        email,
        "Back Up your wallet configuration (Descriptor)",
        Column::new()
            .push(
                Column::new()
                    .push(text(prompt::BACKUP_DESCRIPTOR_MESSAGE))
                    .push(collapse::Collapse::new(
                        || {
                            Button::new(
                                Row::new()
                                    .align_y(Alignment::Center)
                                    .spacing(10)
                                    .push(text("Learn more").small().bold())
                                    .push(icon::collapse_icon()),
                            )
                            .style(theme::button::transparent)
                        },
                        || {
                            Button::new(
                                Row::new()
                                    .align_y(Alignment::Center)
                                    .spacing(10)
                                    .push(text("Learn more").small().bold())
                                    .push(icon::collapsed_icon()),
                            )
                            .style(theme::button::transparent)
                        },
                        help_backup,
                    ))
                    .max_width(1000),
            )
            .push(error.map(|e| card::error("Failed to export backup", e.to_string())))
            .push(
                card::simple(
                    Column::new()
                        .push(text("The descriptor:").small().bold())
                        .push(
                            scrollable(
                                Column::new()
                                    .push(text(descriptor.to_string()).small())
                                    .push(Space::new().height(Length::Fixed(5.0))),
                            )
                            .direction(
                                scrollable::Direction::Horizontal(
                                    scrollable::Scrollbar::new().width(5).scroller_width(5),
                                ),
                            ),
                        )
                        .push(
                            Row::new()
                                .push(Space::new().width(Length::Fill))
                                .push(backup_button)
                                .push(Space::new().width(10))
                                .push(
                                    button::secondary(Some(icon::clipboard_icon()), "Copy")
                                        .on_press(Message::Clipboard(descriptor.to_string())),
                                ),
                        )
                        .spacing(10),
                )
                .max_width(1500),
            )
            .push(
                card::simple(display_policy(descriptor.policy(), keys))
                    .width(Length::Fill)
                    .max_width(1500),
            )
            .push(
                checkbox(done)
                    .label("I have backed up my descriptor")
                    .on_toggle(Message::UserActionDone),
            )
            .push(if done {
                button::primary(None, "Next")
                    .on_press(Message::Next)
                    .width(Length::Fixed(200.0))
            } else {
                button::secondary(None, "Next").width(Length::Fixed(200.0))
            })
            .push(Space::new().height(20.0))
            .spacing(50),
        true,
        Some(Message::Previous),
    )
}

fn display_policy(
    policy: CoincubePolicy,
    keys: &HashMap<Fingerprint, settings::KeySetting>,
) -> Element<'_, Message> {
    let (primary_threshold, primary_keys) = policy.primary_path().thresh_origins();
    // The iteration over an HashMap keys can have a different order at each refresh
    let mut primary_keys: Vec<Fingerprint> = primary_keys.into_keys().collect();
    primary_keys.sort();
    let recovery_paths = policy.recovery_paths();
    let mut col = Column::new().push(
        Row::new()
            .spacing(5)
            .push(
                text(format!(
                    "{} signature{}",
                    primary_threshold,
                    if primary_threshold > 1 { "s" } else { "" }
                ))
                .bold(),
            )
            .push(if primary_keys.len() > 1 {
                text(format!("out of {} by", primary_keys.len()))
            } else {
                text("by")
            })
            .push(
                primary_keys
                    .iter()
                    .enumerate()
                    .fold(Row::new().spacing(5), |row, (i, k)| {
                        let content = if let Some(key) = keys.get(k) {
                            Container::new(
                                iced_tooltip::Tooltip::new(
                                    text(key.name.clone()).bold(),
                                    text(k.to_string()),
                                    iced_tooltip::Position::Bottom,
                                )
                                .style(theme::card::simple),
                            )
                        } else {
                            Container::new(text(format!("[{}]", k)).bold())
                        };
                        if primary_keys.len() == 1 || i == primary_keys.len() - 1 {
                            row.push(content)
                        } else if i <= primary_keys.len() - 2 {
                            row.push(content).push(text("and"))
                        } else {
                            row.push(content).push(text(","))
                        }
                    }),
            )
            .push(text("can always spend this wallet's funds (Primary path)")),
    );
    for (i, (sequence, recovery_path)) in recovery_paths.iter().enumerate() {
        let (threshold, recovery_keys) = recovery_path.thresh_origins();
        // The iteration over an HashMap keys can have a different order at each refresh
        let mut recovery_keys: Vec<Fingerprint> = recovery_keys.into_keys().collect();
        recovery_keys.sort();
        col = col.push(
            Row::new()
                .spacing(5)
                .push(
                    text(format!(
                        "{} signature{}",
                        threshold,
                        if threshold > 1 { "s" } else { "" }
                    ))
                    .bold(),
                )
                .push(if recovery_keys.len() > 1 {
                    text(format!("out of {} by", recovery_keys.len()))
                } else {
                    text("by")
                })
                .push(recovery_keys.iter().enumerate().fold(
                    Row::new().spacing(5),
                    |row, (i, k)| {
                        let content = if let Some(key) = keys.get(k) {
                            Container::new(
                                iced_tooltip::Tooltip::new(
                                    text(key.name.clone()).bold(),
                                    text(k.to_string()),
                                    iced_tooltip::Position::Bottom,
                                )
                                .style(theme::card::simple),
                            )
                        } else {
                            Container::new(text(format!("[{}]", k)).bold())
                        };
                        if recovery_keys.len() == 1 || i == recovery_keys.len() - 1 {
                            row.push(content)
                        } else if i <= recovery_keys.len() - 2 {
                            row.push(content).push(text("and"))
                        } else {
                            row.push(content).push(text(","))
                        }
                    },
                ))
                .push(text("can spend coins after a relative timelock of"))
                .push(
                    text(format!(
                        "{} blocks (~{})",
                        sequence,
                        expire_message_units(*sequence as u32).join(",")
                    ))
                    .bold(),
                )
                .push(text(
                    // If max timelock and all keys are from provider, then it's a safety net path.
                    if *sequence == u16::MAX
                        && recovery_keys
                            .iter()
                            .all(|fg| keys.get(fg).is_some_and(|k| k.provider_key.is_some()))
                    {
                        "(Safety Net path)".to_string()
                    } else {
                        format!("(Recovery path #{})", i + 1)
                    },
                )),
        );
    }
    Column::new()
        .spacing(10)
        .push(text("The wallet policy:").bold())
        .push(scrollable(col).direction(scrollable::Direction::Horizontal(
            scrollable::Scrollbar::new().width(5).scroller_width(5),
        )))
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

pub fn help_backup<'a>() -> Element<'a, Message> {
    text(prompt::BACKUP_DESCRIPTOR_HELP).small().into()
}

pub fn define_bitcoin_node<'a>(
    progress: (usize, usize),
    available_node_types: impl Iterator<Item = NodeType>,
    selected_node_type: NodeType,
    node_view: Element<'a, Message>,
    is_running: Option<&Result<(), Error>>,
    can_try_ping: bool,
    waiting_for_ping_result: bool,
) -> Element<'a, Message> {
    let col = Column::new()
        .push(
            available_node_types.fold(
                Row::new()
                    .push(text("Node type:").small().bold())
                    .spacing(10),
                |row, node_type| {
                    row.push(radio(
                        match node_type {
                            NodeType::Bitcoind => "Bitcoin Core",
                            NodeType::Electrum => "Electrum",
                            NodeType::Esplora => "Esplora",
                        },
                        node_type,
                        Some(selected_node_type),
                        |new_selection| {
                            Message::DefineNode(message::DefineNode::NodeTypeSelected(
                                new_selection,
                            ))
                        },
                    ))
                    .spacing(30)
                    .align_y(Alignment::Center)
                },
            ),
        )
        .push(node_view)
        .push(if waiting_for_ping_result {
            Some(Container::new(
                Row::new()
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .push(text("Checking connection...")),
            ))
        } else if is_running.is_some() {
            is_running.map(|res| {
                if res.is_ok() {
                    Container::new(
                        Row::new()
                            .spacing(10)
                            .align_y(Alignment::Center)
                            .push(icon::square_check_icon().style(theme::text::success))
                            .push(text("Connection checked").style(theme::text::success)),
                    )
                } else {
                    let detail = match res {
                        Ok(_) => String::new(),
                        Err(e) => e.to_string(),
                    };
                    Container::new(
                        Row::new()
                            .spacing(10)
                            .align_y(Alignment::Center)
                            .push(icon::square_cross_icon().style(theme::text::error))
                            .push(
                                text(format!("Connection failed: {detail}"))
                                    .style(theme::text::error),
                            ),
                    )
                }
            })
        } else {
            Some(Container::new(Space::new().height(Length::Fixed(21.0))))
        })
        .push(
            Row::new()
                .spacing(10)
                .push(Container::new(
                    button::secondary(None, "Check connection")
                        .on_press_maybe(if can_try_ping && !waiting_for_ping_result {
                            Some(Message::DefineNode(DefineNode::Ping))
                        } else {
                            None
                        })
                        .width(Length::Fixed(200.0)),
                ))
                .push(if is_running.map(|res| res.is_ok()).unwrap_or(false) {
                    button::secondary(None, "Next")
                        .on_press(Message::Next)
                        .width(Length::Fixed(200.0))
                } else {
                    button::secondary(None, "Next").width(Length::Fixed(200.0))
                }),
        )
        .spacing(50);

    layout(
        progress,
        None,
        "Set up connection to the Bitcoin node",
        col,
        true,
        Some(Message::Previous),
    )
}

pub fn define_bitcoind<'a>(
    address: &form::Value<String>,
    rpc_auth_vals: &RpcAuthValues,
    selected_auth_type: &RpcAuthType,
) -> Element<'a, Message> {
    let is_loopback = if let Some((ip, _port)) = address.value.clone().rsplit_once(':') {
        let (ipv4, ipv6) = (Ipv4Addr::from_str(ip), Ipv6Addr::from_str(ip));
        match (ipv4, ipv6) {
            (_, Ok(ip)) => ip.is_loopback(),
            (Ok(ip), _) => ip.is_loopback(),
            _ => false,
        }
    } else {
        false
    };

    let col_address = Column::new()
        .push(text("Address:").bold())
        .push(
            form::Form::new_trimmed("Address", address, |msg| {
                Message::DefineNode(DefineNode::DefineBitcoind(
                    DefineBitcoind::ConfigFieldEdited(ConfigField::Address, msg),
                ))
            })
            .warning("Please enter correct address")
            .size(text::P1_SIZE)
            .padding(10),
        )
        .push(if !is_loopback && address.valid {
            Some(
                iced::widget::Text::new(
                    "Connection to a remote Bitcoin node \
                    is not supported. Insert an IP address bound to the same machine \
                    running Coincube (ignore this warning if that's already the case)",
                )
                .style(theme::text::warning)
                .size(text::CAPTION_SIZE),
            )
        } else {
            None
        })
        .spacing(10);

    let col_auth = Column::new()
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
                            |new_selection| {
                                Message::DefineNode(DefineNode::DefineBitcoind(
                                    DefineBitcoind::RpcAuthTypeSelected(new_selection),
                                ))
                            },
                        ))
                        .spacing(30)
                        .align_y(Alignment::Center)
                    },
                ),
        )
        .push(match selected_auth_type {
            RpcAuthType::CookieFile => Row::new().push(
                form::Form::new_trimmed("Cookie path", &rpc_auth_vals.cookie_path, |msg| {
                    Message::DefineNode(DefineNode::DefineBitcoind(
                        DefineBitcoind::ConfigFieldEdited(ConfigField::CookieFilePath, msg),
                    ))
                })
                .warning("Please enter correct path")
                .size(text::P1_SIZE)
                .padding(10),
            ),
            RpcAuthType::UserPass => Row::new()
                .push(
                    form::Form::new_trimmed("User", &rpc_auth_vals.user, |msg| {
                        Message::DefineNode(DefineNode::DefineBitcoind(
                            DefineBitcoind::ConfigFieldEdited(ConfigField::User, msg),
                        ))
                    })
                    .warning("Please enter correct user")
                    .size(text::P1_SIZE)
                    .padding(10),
                )
                .push(
                    form::Form::new_trimmed("Password", &rpc_auth_vals.password, |msg| {
                        Message::DefineNode(DefineNode::DefineBitcoind(
                            DefineBitcoind::ConfigFieldEdited(ConfigField::Password, msg),
                        ))
                    })
                    .warning("Please enter correct password")
                    .size(text::P1_SIZE)
                    .padding(10),
                )
                .spacing(10),
        })
        .spacing(10);

    Column::new()
        .push(col_address)
        .push(col_auth)
        .spacing(50)
        .into()
}

pub fn define_electrum<'a>(
    address: &form::Value<String>,
    validate_domain: bool,
) -> Element<'a, Message> {
    let checkbox = validate_domain_checkbox(address, validate_domain, |b| {
        Message::DefineNode(DefineNode::DefineElectrum(
            message::DefineElectrum::ValidDomainChanged(b),
        ))
    });
    let col_address = Column::new()
        .push(text("Address:").bold())
        .push(
            form::Form::new_trimmed("127.0.0.1:50001", address, |msg| {
                Message::DefineNode(DefineNode::DefineElectrum(
                    message::DefineElectrum::ConfigFieldEdited(electrum::ConfigField::Address, msg),
                ))
            })
            .warning(
                "Please enter correct address (including port), \
                optionally prefixed with tcp:// or ssl://",
            )
            .size(text::P1_SIZE)
            .padding(10),
        )
        .push(checkbox)
        .push(text(electrum::ADDRESS_NOTES))
        .spacing(10);

    Column::new().push(col_address).spacing(50).into()
}

pub fn define_esplora<'a>(
    address: &form::Value<String>,
    placeholder: &'a str,
) -> Element<'a, Message> {
    let col_address = Column::new()
        .push(text("Esplora URL:").bold())
        .push(
            form::Form::new_trimmed(placeholder, address, |msg| {
                Message::DefineNode(DefineNode::DefineEsplora(DefineEsplora::ConfigFieldEdited(
                    esplora::ConfigField::Address,
                    msg,
                )))
            })
            .warning("Please enter a valid URL starting with http:// or https://")
            .size(text::P1_SIZE)
            .padding(10),
        )
        .spacing(10);

    Column::new().push(col_address).spacing(50).into()
}

#[allow(clippy::too_many_arguments)]
pub fn define_coincube_connect<'a>(
    progress: (usize, usize),
    email: &form::Value<String>,
    otp: &form::Value<String>,
    otp_sent: bool,
    is_signup: bool,
    processing: bool,
    error: Option<&'a str>,
    allow_skip: bool,
) -> Element<'a, Message> {
    let content: Element<'a, Message> = if !otp_sent {
        let (action_label, toggle_label) = if is_signup {
            ("Create Account", "Already have an account? Sign in")
        } else {
            ("Sign In", "New to COINCUBE | Connect? Create account")
        };
        Column::new()
            .spacing(20)
            .push(text(
                "Create a free COINCUBE | Connect account to unlock:",
            ))
            .push(
                Column::new()
                    .spacing(8)
                    .push(text("  •  Hosted Esplora server — use your Vault instantly while your node syncs"))
                    .push(text("  •  Encrypted wallet descriptor backup"))
                    .push(text("  •  Integration with the companion Keychain mobile app")),
            )
            .push(text(
                "Enter your email address to receive a verification code.",
            ))
            .push(
                form::Form::new_trimmed("your@email.com", email, |msg| {
                    Message::CoincubeConnect(CoincubeConnectMsg::EmailEdited(msg))
                })
                .warning("Please enter a valid email address")
                .size(text::P1_SIZE)
                .padding(10),
            )
            .push(error.map(|e| text(e).style(theme::text::warning)))
            .push(
                button::secondary(None, action_label)
                    .width(Length::Fixed(250.0))
                    .on_press_maybe(if !processing && email.valid {
                        Some(Message::CoincubeConnect(CoincubeConnectMsg::RequestOtp))
                    } else {
                        None
                    }),
            )
            .push(
                button::transparent(None, toggle_label)
                    .on_press(Message::CoincubeConnect(CoincubeConnectMsg::ToggleMode)),
            )
            .push(Space::new().height(Length::Fixed(10.0)))
            .push(allow_skip.then(|| button::transparent(None, "Skip for now")
                    .on_press(Message::CoincubeConnect(CoincubeConnectMsg::Skip))))
            .into()
    } else {
        Column::new()
            .spacing(20)
            .push(text(format!(
                "Enter the verification code sent to {}",
                email.value
            )))
            .push(
                form::Form::new_trimmed("6-digit code", otp, |msg| {
                    Message::CoincubeConnect(CoincubeConnectMsg::OtpEdited(msg))
                })
                .warning("Invalid verification code")
                .size(text::P1_SIZE)
                .padding(10),
            )
            .push(error.map(|e| text(e).style(theme::text::warning)))
            .push(
                button::secondary(None, "Resend Code")
                    .width(Length::Fixed(200.0))
                    .on_press_maybe(if !processing {
                        Some(Message::CoincubeConnect(CoincubeConnectMsg::ResendOtp))
                    } else {
                        None
                    }),
            )
            .into()
    };

    layout(
        progress,
        None,
        "COINCUBE | Connect",
        content,
        true,
        Some(Message::Previous),
    )
}

/// Node-resource controls (prune target + mempool cap) shared by the installer's
/// advanced disclosure and the Vault node settings, so the two surfaces never
/// drift. Presets are a thin setter layer over the two custom MB fields:
/// `on_prune`/`on_mempool` receive the raw MB string the widget produces (an
/// empty mempool string means "leave blank" = bitcoind's 300 MB default =
/// `max_mempool_mb: None`). `small_computer` and `regular_computer` are the
/// one-click machine-profile presets that set both fields at once — 550 MB prune
/// with a 100 MB mempool cap, and the 15 GB / 300 MB defaults respectively. The
/// estimated-total line is honest about the unprunable chainstate floor — the
/// prune choice only bounds block data.
#[allow(clippy::too_many_arguments)]
pub fn node_resources_controls<'a, M>(
    prune: &'a form::Value<String>,
    max_mempool: &'a form::Value<String>,
    on_prune: impl Fn(String) -> M + Clone + 'static,
    on_mempool: impl Fn(String) -> M + Clone + 'static,
    small_computer: M,
    regular_computer: M,
) -> Column<'a, M>
where
    M: Clone + 'a,
{
    use crate::node::bitcoind::{
        estimated_total_disk_gb, MAX_MEMPOOL_DEFAULT_MB, MAX_MEMPOOL_SMALL_MB, PRUNE_COMPACT_MB,
        PRUNE_DEFAULT, PRUNE_MINIMAL_MB,
    };

    // Current values back the honest disk/memory estimate, updating live as the
    // user types. A blank or unparseable field falls back to the default so the
    // line is always meaningful; a parseable-but-below-floor value (e.g. 100) is
    // shown as-is here and caught by validation on apply.
    let prune_mb = prune.value.parse::<u32>().unwrap_or(PRUNE_DEFAULT);
    let mem_mb = if max_mempool.value.is_empty() {
        MAX_MEMPOOL_DEFAULT_MB
    } else {
        max_mempool
            .value
            .parse::<u32>()
            .unwrap_or(MAX_MEMPOOL_DEFAULT_MB)
    };
    // "Default" mempool is the blank (key-omitted) state; "Small" is exactly the
    // 100 MB cap. Used to highlight both the mempool presets and the one-click
    // machine-profile buttons.
    let mem_is_default = max_mempool.value.is_empty();
    let mem_is_small = max_mempool.value.parse::<u32>().ok() == Some(MAX_MEMPOOL_SMALL_MB);

    // Prune presets: highlight the one matching the current value.
    let prune_preset = |label: &'static str, val: u32, on: &dyn Fn() -> M| {
        let btn = if prune_mb == val {
            button::primary(None, label)
        } else {
            button::secondary(None, label)
        };
        btn.width(Length::Fixed(120.0)).on_press(on())
    };
    let prune_row = Row::new()
        .spacing(10)
        .push(prune_preset("Minimal", PRUNE_MINIMAL_MB, &|| {
            on_prune(PRUNE_MINIMAL_MB.to_string())
        }))
        .push(prune_preset("Compact", PRUNE_COMPACT_MB, &|| {
            on_prune(PRUNE_COMPACT_MB.to_string())
        }))
        .push(prune_preset("Default", PRUNE_DEFAULT, &|| {
            on_prune(PRUNE_DEFAULT.to_string())
        }));
    // Custom prune field last, so `on_prune` can be moved into it.
    let prune_field = Container::new(Element::from(form::Form::new_amount_sats(
        "15000", prune, on_prune,
    )))
    .width(Length::Fixed(140.0));

    // Mempool presets: "Default" writes an empty string (key omitted); highlight
    // it whenever the field is blank.
    let small_btn = if mem_is_small {
        button::primary(None, "Small (100 MB)")
    } else {
        button::secondary(None, "Small (100 MB)")
    }
    .width(Length::Fixed(160.0))
    .on_press(on_mempool(MAX_MEMPOOL_SMALL_MB.to_string()));
    let default_btn = if mem_is_default {
        button::primary(None, "Default (300 MB)")
    } else {
        button::secondary(None, "Default (300 MB)")
    }
    .width(Length::Fixed(160.0))
    .on_press(on_mempool(String::new()));
    let mem_row = Row::new().spacing(10).push(small_btn).push(default_btn);
    let mem_field = Container::new(Element::from(form::Form::new_amount_sats(
        "300",
        max_mempool,
        on_mempool,
    )))
    .width(Length::Fixed(140.0));

    Column::new()
        .spacing(12)
        .push(text("Node data to keep").bold())
        .push(
            text(
                "How much block data your node keeps on disk. The chainstate \
                 (~14 GB) is always kept and can't be pruned, so total disk use \
                 is always higher than this number.",
            )
            .size(12)
            .style(theme::text::secondary),
        )
        .push(prune_row)
        .push(
            Row::new()
                .spacing(10)
                .align_y(Alignment::Center)
                .push(text("Custom (MB):"))
                .push(prune_field),
        )
        .push(Space::new().height(Length::Fixed(8.0)))
        .push(text("Mempool memory cap").bold())
        .push(
            text(
                "Memory reserved for unconfirmed transactions. During heavy \
                 congestion a small cap can delay when small incoming payments \
                 first appear, until they confirm.",
            )
            .size(12)
            .style(theme::text::secondary),
        )
        .push(mem_row)
        .push(
            Row::new()
                .spacing(10)
                .align_y(Alignment::Center)
                .push(text("Custom (MB):"))
                .push(mem_field)
                .push(
                    text("Leave blank for the default (300 MB).")
                        .size(12)
                        .style(theme::text::secondary),
                ),
        )
        .push(Space::new().height(Length::Fixed(8.0)))
        // One-click machine profiles. "Small computer" = 550 MB prune + 100 MB
        // mempool; "Regular computer" = the 15 GB / 300 MB defaults. Highlighted
        // when the current values match. A future "Miner" preset joins this row.
        .push(
            Row::new()
                .spacing(10)
                .push(
                    if prune_mb == PRUNE_MINIMAL_MB && mem_is_small {
                        button::primary(None, "Small computer")
                    } else {
                        button::secondary(None, "Small computer")
                    }
                    .width(Length::Fixed(180.0))
                    .on_press(small_computer),
                )
                .push(
                    if prune_mb == PRUNE_DEFAULT && mem_is_default {
                        button::primary(None, "Regular computer")
                    } else {
                        button::secondary(None, "Regular computer")
                    }
                    .width(Length::Fixed(180.0))
                    .on_press(regular_computer),
                ),
        )
        .push(
            text(format!(
                "Estimated total: about {} GB of disk, and up to {} MB of memory \
                 for the mempool.",
                estimated_total_disk_gb(prune_mb),
                mem_mb,
            ))
            .size(12)
            .style(theme::text::secondary),
        )
}

/// Knots / Core picker shown on the node-management step when the user opts to
/// install a managed node. Knots is the default; the selected flavour is
/// primary-styled.
///
/// The choice determines both which node software COINCUBE downloads and runs
/// and, with it, the relay policy that node applies — which is what the one-line
/// blurb describes, since the two builds follow the same consensus rules.
fn node_flavor_selector<'a>(
    ui_instance: u64,
    selected: crate::node::bitcoind::NodeFlavor,
    existing: Option<crate::node::bitcoind::NodeFlavor>,
    on_select: fn(crate::node::bitcoind::NodeFlavor) -> Message,
) -> Element<'a, Message> {
    use crate::node::bitcoind::NodeFlavor;
    if selected == NodeFlavor::KnotsBlake2b {
        return Column::new().spacing(10)
            .push(text("Bitcoin Knots for Bitcoin Blake2b").bold())
            .push(text("A pruned node, stored separately from your Bitcoin node. Vaults on this chain share it. Connections are outbound only; inbound Tor is unavailable."))
            .into();
    }
    let option = |flavor: NodeFlavor| {
        let label = flavor.display_name();
        let id = if flavor == NodeFlavor::Knots {
            "knots"
        } else {
            "core"
        };
        let action = Action::new(
            format!("tenshu.installer.node.{}.flavor.{}", ui_instance, id),
            p1_regular(label),
            Message::FocusAction,
        );
        let action = if selected == flavor {
            action.style(theme::button::primary)
        } else {
            action.style(theme::button::secondary)
        };
        action.width(Length::Fill).on_press(on_select(flavor))
    };
    let blurb = match selected {
        NodeFlavor::Knots => {
            "Bitcoin Knots — stricter relay defaults; it limits oversized data-carrier \
             (\"spam\") transactions. Standard sends are unaffected. Recommended."
        }
        NodeFlavor::Core => {
            "Bitcoin Core — the reference implementation, with its default relay policy."
        }
        // The installer only ever lists `NodeChainFamily::Bitcoin` flavours;
        // this arm exists for exhaustiveness and says so if it is ever hit.
        NodeFlavor::KnotsBlake2b => {
            "Bitcoin Knots (Bitcoin Blake2b) — serves the Bitcoin Blake2b chain only; \
             not available for a Bitcoin node."
        }
    };
    let mut col = Column::new()
        .spacing(10)
        .push(text("Node software").bold())
        .push(
            Column::new()
                .spacing(10)
                .push(option(NodeFlavor::Knots))
                .push(option(NodeFlavor::Core)),
        )
        .push(text(blurb))
        // The managed node is shared across every Vault, so the choice is global.
        .push(text(
            "All your Vaults share one Bitcoin node, so this choice applies to every Vault.",
        ));
    // When a node already exists and this would change its flavour, make the
    // global impact explicit: switching restarts the node for ALL Vaults.
    if let Some(existing) = existing {
        if existing != selected {
            col = col.push(
                text(format!(
                    "Changing this will restart your node and switch all existing Vaults to {}.",
                    selected.display_name()
                ))
                .bold(),
            );
        }
    }
    col.into()
}

fn node_option_card<'a>(
    ui_instance: u64,
    id: &'static str,
    title: &'static str,
    description: impl Into<String>,
    action: message::SelectBitcoindTypeMsg,
) -> Element<'a, Message> {
    Container::new(
        Column::new()
            .spacing(12)
            .width(Length::Fill)
            .push(p1_bold(title))
            .push(p2_regular(description.into()).style(theme::text::secondary))
            .push(
                Action::new(
                    format!("tenshu.installer.node.{}.{}", ui_instance, id),
                    p1_regular("Select"),
                    Message::FocusAction,
                )
                .style(theme::button::secondary)
                .on_press(Message::SelectBitcoindType(action)),
            ),
    )
    .padding(20)
    .width(Length::Fill)
    .style(theme::container::border_grey)
    .into()
}

fn node_install_toggle<'a>(
    ui_instance: u64,
    install_node: bool,
    label: &'static str,
) -> Element<'a, Message> {
    let (mark, state_and_action) = if install_node {
        ("☑", "On — activate to turn Off")
    } else {
        ("☐", "Off — activate to turn On")
    };
    // Iced 0.14's checkbox is not keyboard focusable. The dynamic button text
    // names both the current choice and the result of activating it.
    Action::new(
        format!("tenshu.installer.node.{}.install", ui_instance),
        p1_regular(format!("{} {}: {}", mark, label, state_and_action)),
        Message::FocusAction,
    )
    .style(theme::button::transparent)
    .on_press(Message::SelectBitcoindType(
        message::SelectBitcoindTypeMsg::ToggleInstallNode,
    ))
    .into()
}

#[allow(clippy::too_many_arguments)]
pub fn select_bitcoind_type<'a>(
    progress: (usize, usize),
    ui_instance: u64,
    chain: crate::chain::ChainId,
    install_node: bool,
    show_advanced: bool,
    prune_default_mb: u32,
    connect_authenticated: bool,
    node_flavor: crate::node::bitcoind::NodeFlavor,
    existing_flavor: Option<crate::node::bitcoind::NodeFlavor>,
    local_available: bool,
) -> Element<'a, Message> {
    use crate::chain::ChainIdExt;
    if chain.is_blake2b() {
        let chain_name = chain.label();
        let mut choices = Column::new().spacing(20).max_width(620);
        if connect_authenticated || !local_available {
            choices = choices
                .push(text("Use COINCUBE | Connect").bold())
                .push(text(format!("Use the hosted {chain_name} node while a local node syncs, or use Connect alone.")))
                .push(node_install_toggle(ui_instance, install_node, "Also install a pruned node on my device"))
                .push(Action::new(format!("tenshu.installer.node.{}.blake2b-connect", ui_instance), p1_regular("Use Connect"), Message::FocusAction)
                    .style(theme::button::primary)
                    .on_press(Message::SelectBitcoindType(message::SelectBitcoindTypeMsg::ContinueWithConnect)));
        }
        if local_available {
            choices = choices
            .push(text("Install a local pruned node").bold())
            .push(text(format!("Run Bitcoin Knots for {chain_name} in an isolated data directory. No Connect account is required. Your node must finish syncing before the Vault can be created.")))
            .push(Action::new(format!("tenshu.installer.node.{}.blake2b-local", ui_instance), p1_regular("Use a local node"), Message::FocusAction)
                .style(theme::button::secondary)
                .on_press(Message::SelectBitcoindType(message::SelectBitcoindTypeMsg::UseExternal(false))));
        }
        let content = Container::new(choices).padding(30);
        return layout(
            progress,
            None,
            if chain == crate::chain::ChainId::BitcoinBlake2b {
                "Bitcoin Blake2b node management"
            } else {
                "Bitcoin Blake2b Testnet4 node management"
            },
            content,
            true,
            Some(Message::Previous),
        );
    }
    let network = chain.bitcoin_network();
    let content: Column<'a, Message> = if network == bitcoin::Network::Regtest
        || !connect_authenticated
    {
        // A vertical choice list fits the compact installer viewport and
        // keeps each description next to the action it explains.
        let local_description = if network == bitcoin::Network::Regtest {
            "Tenshu will install a pruned node on your computer. No local node required beforehand."
                .to_owned()
        } else {
            let disk_gb = (prune_default_mb as f64 / 1024.0).round() as u32;
            format!(
                    "Tenshu will download and configure a pruned Bitcoin node on your computer (~{} GB of disk space required).",
                    disk_gb
                )
        };
        Column::new()
            .spacing(12)
            .max_width(620)
            .push(node_option_card(
                ui_instance,
                "existing",
                "I already have a node",
                "Connect Tenshu to a Bitcoin node already running locally or remotely.",
                message::SelectBitcoindTypeMsg::UseExternal(true),
            ))
            .push(node_option_card(
                ui_instance,
                "local",
                "Install a local pruned node",
                local_description,
                message::SelectBitcoindTypeMsg::UseExternal(false),
            ))
    } else {
        // Connect authenticated: opinionated default card (Connect + optional node)
        // plus a collapsible Advanced section for power users.
        let node_description = if install_node {
            let disk_gb = (prune_default_mb as f64 / 1024.0).round() as u32;
            format!(
                "Tenshu will download and configure a pruned {} node \
                (~{} GB of disk space required). \
                Once synced, your Vault will automatically switch to your local node.",
                node_flavor.display_name(),
                disk_gb
            )
        } else {
            "Your Vault will use COINCUBE | Connect as its only Bitcoin backend.".to_string()
        };

        let default_col = Column::new()
            .spacing(20)
            .max_width(620)
            .push(text("Start with COINCUBE | Connect").bold())
            .push(text(
                "Your Vault will use our hosted Esplora server immediately \
                    (backed by Bitcoin Knots). No setup required.",
            ))
            .push(node_install_toggle(
                ui_instance,
                install_node,
                "Also install a pruned Bitcoin node on my device",
            ));

        let default_col = if install_node {
            default_col.push(node_flavor_selector(
                ui_instance,
                node_flavor,
                existing_flavor,
                |flavor| {
                    Message::SelectBitcoindType(message::SelectBitcoindTypeMsg::SelectFlavor(
                        flavor,
                    ))
                },
            ))
        } else {
            default_col
        };

        let default_card = Container::new(
            default_col.push(text(node_description)).push(
                Action::new(
                    format!("tenshu.installer.node.{}.continue", ui_instance),
                    p1_regular("Continue"),
                    Message::FocusAction,
                )
                .style(theme::button::primary)
                .on_press(Message::SelectBitcoindType(
                    message::SelectBitcoindTypeMsg::ContinueWithConnect,
                )),
            ),
        )
        .padding(30);

        let toggle_label = if show_advanced {
            "▾  Advanced options"
        } else {
            "▸  Advanced options"
        };
        let advanced_toggle = Action::new(
            format!("tenshu.installer.node.{}.advanced", ui_instance),
            p1_regular(toggle_label),
            Message::FocusAction,
        )
        .style(theme::button::transparent)
        .on_press(Message::SelectBitcoindType(
            message::SelectBitcoindTypeMsg::ToggleAdvanced,
        ));

        let mut col = Column::new()
            .spacing(30)
            .push(default_card)
            .push(advanced_toggle);

        if show_advanced {
            col = col
                    .push(node_option_card(
                        ui_instance,
                        "advanced-existing",
                        "I already have a node",
                        "Connect Tenshu to a Bitcoin node already running locally or remotely.",
                        message::SelectBitcoindTypeMsg::UseExternal(true),
                    ))
                    .push(node_option_card(
                        ui_instance,
                        "advanced-local",
                        "Install a node only",
                        "Tenshu installs a pruned node on your computer without COINCUBE | Connect as a fallback.",
                        message::SelectBitcoindTypeMsg::UseExternal(false),
                    ))
                    .push(node_option_card(
                        ui_instance,
                        "advanced-connect",
                        "COINCUBE | Connect only",
                        "Use COINCUBE | Connect to access our Esplora. No local node required.",
                        message::SelectBitcoindTypeMsg::UseConnect,
                    ));
        }

        col
    };

    layout(
        progress,
        None,
        "Bitcoin node management",
        content,
        true,
        Some(Message::Previous),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn start_internal_bitcoind<'a>(
    progress: (usize, usize),
    ui_instance: u64,
    flavor: crate::node::bitcoind::NodeFlavor,
    existing_flavor: Option<crate::node::bitcoind::NodeFlavor>,
    flavor_confirmed: bool,
    show_advanced: bool,
    prune: &'a form::Value<String>,
    max_mempool: &'a form::Value<String>,
    exe_path: Option<&PathBuf>,
    started: Option<&Result<(), StartInternalBitcoindError>>,
    error: Option<&'a String>,
    download_state: Option<&DownloadState>,
    install_state: Option<&InstallState>,
) -> Element<'a, Message> {
    let version = flavor.version();
    let flavor_name = flavor.display_name();

    // Choose the node software first; nothing is downloaded or started until the
    // user confirms here. The advanced disclosure lets the user tune node
    // resources (prune target + mempool cap) before the node is configured — the
    // chosen values are threaded into `DefineConfig` (this same step).
    if !flavor_confirmed {
        let adv_label = if show_advanced {
            "▾  Advanced options"
        } else {
            "▸  Advanced options"
        };
        let mut col = Column::new()
            .spacing(25)
            .push(node_flavor_selector(
                ui_instance,
                flavor,
                existing_flavor,
                |flavor| {
                    Message::InternalBitcoind(message::InternalBitcoindMsg::SelectFlavor(flavor))
                },
            ))
            .push(
                button::transparent(None, adv_label).on_press(Message::InternalBitcoind(
                    message::InternalBitcoindMsg::ToggleAdvanced,
                )),
            );
        if show_advanced {
            col = col.push(card::simple(node_resources_controls(
                prune,
                max_mempool,
                |v| Message::InternalBitcoind(message::InternalBitcoindMsg::PruneEdited(v)),
                |v| Message::InternalBitcoind(message::InternalBitcoindMsg::MaxMempoolEdited(v)),
                Message::InternalBitcoind(message::InternalBitcoindMsg::SmallComputerPreset),
                Message::InternalBitcoind(message::InternalBitcoindMsg::RegularComputerPreset),
            )));
        }
        col = col.push(
            Row::new().push(
                button::secondary(None, "Download & start")
                    .width(Length::Fixed(220.0))
                    .on_press(Message::InternalBitcoind(
                        message::InternalBitcoindMsg::ConfirmFlavor,
                    )),
            ),
        );
        return layout(
            progress,
            None,
            if flavor == crate::node::bitcoind::NodeFlavor::KnotsBlake2b {
                "Start a pruned Bitcoin Blake2b node"
            } else {
                "Start Bitcoin full node"
            },
            col,
            true,
            Some(message::Message::InternalBitcoind(
                message::InternalBitcoindMsg::Previous,
            )),
        );
    }

    layout(
        progress,
        None,
        if flavor == crate::node::bitcoind::NodeFlavor::KnotsBlake2b {
            "Start a pruned Bitcoin Blake2b node"
        } else {
            "Start Bitcoin full node"
        },
        Column::new()
            .push(download_state.map(|s| {
                match s {
                    DownloadState::Finished(_) => Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push(icon::square_check_icon().style(theme::text::success))
                        .push(text("Download complete").style(theme::text::success)),
                    DownloadState::Downloading { progress, .. } => Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push(text(format!(
                            "Downloading {flavor_name} {version}... {progress:.2}%"
                        ))),
                    DownloadState::Errored(e) => Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push(icon::square_cross_icon().style(theme::text::error))
                        .push(text(format!("Download failed: '{}'.", e)).style(theme::text::error)),
                    _ => Row::new().spacing(10).align_y(Alignment::Center),
                }
            }))
            .push(Container::new(if let Some(state) = install_state {
                match state {
                    InstallState::InProgress => Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push("Installing bitcoind..."),
                    InstallState::Finished => Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push(icon::square_check_icon().style(theme::text::success))
                        .push(text("Installation complete").style(theme::text::success)),
                    InstallState::Errored(e) => Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push(icon::square_cross_icon().style(theme::text::error))
                        .push(
                            text(format!("Installation failed: '{}'.", e))
                                .style(theme::text::error),
                        ),
                }
            } else if exe_path.is_some() {
                Row::new()
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .push(icon::square_check_icon().style(theme::text::success))
                    .push(
                        text("Tenshu-managed bitcoind already installed")
                            .style(theme::text::success),
                    )
            } else if let Some(DownloadState::Downloading { progress, .. }) = download_state {
                Row::new()
                    .spacing(10)
                    .align_y(Alignment::Center)
                    .push(progress_bar(0.0..=100.0, *progress))
            } else {
                Row::new().spacing(10).align_y(Alignment::Center)
            }))
            .push(if started.is_some() {
                started.map(|res| {
                    if res.is_ok() {
                        Container::new(
                            Row::new()
                                .spacing(10)
                                .align_y(Alignment::Center)
                                .push(icon::square_check_icon().style(theme::text::success))
                                .push(text("Started").style(theme::text::success)),
                        )
                    } else {
                        Container::new(
                            Row::new()
                                .spacing(10)
                                .align_y(Alignment::Center)
                                .push(icon::square_cross_icon().style(theme::text::error))
                                .push(
                                    text(res.as_ref().err().unwrap().to_string())
                                        .style(theme::text::error),
                                ),
                        )
                    }
                })
            } else {
                match (install_state, exe_path) {
                    // We have either just installed bitcoind or it was already installed.
                    (Some(InstallState::Finished), _) | (None, Some(_)) => Some(Container::new(
                        Row::new()
                            .spacing(10)
                            .align_y(Alignment::Center)
                            .push(text("Starting...")),
                    )),
                    _ => Some(Container::new(Space::new().height(Length::Fixed(25.0)))),
                }
            })
            .spacing(50)
            .push(
                Row::new().push(
                    button::secondary(None, "Next")
                        .width(Length::Fixed(200.0))
                        .on_press_maybe(if let Some(Ok(_)) = started {
                            Some(Message::Next)
                        } else {
                            None
                        }),
                ),
            )
            .push(error.map(|e| card::invalid(text(e)))),
        true,
        Some(message::Message::InternalBitcoind(
            message::InternalBitcoindMsg::Previous,
        )),
    )
}

pub fn claim_setup_install<'a>(
    progress: (usize, usize),
    email: Option<&'a str>,
    generating: bool,
    warning: Option<&'a String>,
) -> Element<'a, Message> {
    layout(
        progress,
        email,
        "Prepare your Blake2b Cube",
        Column::new().spacing(20)
            .push(p1_regular(if generating {
                "Creating the matching Blake2b Vault…"
            } else {
                "Blake2b Cube setup. The next step opens Bitcoin Claim in your original Cube."
            }))
            .push(warning.map(|e| card::invalid(text(e))))
            .push(p1_regular("Setup does not submit a claim transaction. You’ll review the Bitcoin split and its network fee next.")),
        true,
        (!generating && warning.is_some()).then_some(Message::Previous),
    )
}

pub fn install<'a>(
    progress: (usize, usize),
    email: Option<&'a str>,
    generating: bool,
    installed: bool,
    warning: Option<&'a String>,
    connect_vault_caption: Option<String>,
) -> Element<'a, Message> {
    let prev_msg = if !generating && warning.is_some() {
        Some(Message::Previous)
    } else {
        None
    };
    layout(
        progress,
        email,
        "Finalize installation",
        Column::new()
            .push(warning.map(|e| card::invalid(text(e))))
            .push(if generating {
                Container::new(text("Installing..."))
            } else if installed {
                Container::new(
                    Row::new()
                        .spacing(10)
                        .align_y(Alignment::Center)
                        .push(icon::square_check_icon().style(theme::text::success))
                        .push(text("Installed").style(theme::text::success)),
                )
            } else {
                Container::new(Space::new().height(Length::Fixed(25.0)))
            })
            .push(connect_vault_caption.map(|caption| {
                card::simple(text(caption).style(theme::text::secondary)).padding(10)
            }))
            .spacing(10)
            .width(Length::Fill),
        true,
        prev_msg,
    )
}

pub fn defined_threshold<'a>(
    color: iced::Color,
    fixed: bool,
    threshold: (usize, usize),
) -> Element<'a, message::DefinePath> {
    if !fixed && threshold.1 > 1 {
        Button::new(
            Row::new()
                .spacing(10)
                .push((0..threshold.1).fold(Row::new(), |row, i| {
                    if i < threshold.0 {
                        row.push(icon::round_key_icon().style(theme::text::adaptive(color)))
                    } else {
                        row.push(icon::round_key_icon())
                    }
                }))
                .push(text(format!(
                    "{} out of {} key{}",
                    threshold.0,
                    threshold.1,
                    if threshold.1 > 1 { "s" } else { "" },
                )))
                .push(icon::pencil_icon()),
        )
        .padding(10)
        .on_press(message::DefinePath::EditThreshold)
        .style(theme::button::secondary)
        .into()
    } else {
        card::simple(
            Row::new()
                .spacing(10)
                .push((0..threshold.1).fold(Row::new(), |row, i| {
                    if i < threshold.0 {
                        row.push(icon::round_key_icon().style(theme::text::adaptive(color)))
                    } else {
                        row.push(icon::round_key_icon())
                    }
                }))
                .push(text(format!(
                    "{} out of {} key{}",
                    threshold.0,
                    threshold.1,
                    if threshold.1 > 1 { "s" } else { "" },
                ))),
        )
        .padding(10)
        .into()
    }
}

pub fn defined_sequence<'a>(
    sequence: PathSequence,
    warning: Option<PathWarning>,
) -> Element<'a, message::DefinePath> {
    let duration_row = Row::new()
        .padding(5)
        .spacing(5)
        .align_y(Alignment::Center)
        .push(text(
            format_sequence_duration(sequence.as_u16(), true)
                .iter()
                .filter_map(|(n, unit)| {
                    if *n > 0 {
                        Some(format!("{}{}", n, unit))
                    } else {
                        None
                    }
                })
                .collect::<Vec<String>>()
                .join(" "),
        ));
    Container::new(
        Column::new()
            .spacing(5)
            .push(match sequence {
                PathSequence::Recovery(_) => Row::new().align_y(Alignment::Center).push(
                    Container::new(
                        Row::new()
                            .align_y(Alignment::Center)
                            .spacing(5)
                            .push(
                                text::p1_regular("Available after inactivity of ~")
                                    .style(theme::text::secondary),
                            )
                            .push(
                                Button::new(duration_row.push(icon::pencil_icon()))
                                    .style(theme::button::secondary)
                                    .on_press(message::DefinePath::EditSequence),
                            ),
                    )
                    .width(Length::Fill)
                    .padding(5)
                    .align_y(alignment::Vertical::Center),
                ),
                PathSequence::Primary => Row::new()
                    .push(
                        p1_regular("Able to move the funds at any time.")
                            .style(theme::text::secondary),
                    )
                    .padding(5),
                PathSequence::SafetyNet => Row::new().align_y(Alignment::Center).push(
                    Container::new(
                        Row::new()
                            .align_y(Alignment::Center)
                            .spacing(5)
                            .push(
                                text::p1_regular("Available after inactivity of ~")
                                    .style(theme::text::secondary),
                            )
                            .push(duration_row),
                    )
                    .width(Length::Fill)
                    .padding(5)
                    .align_y(alignment::Vertical::Center),
                ),
            })
            .push(warning.map(|w| {
                text(w.message()).small().style(if w.blocks() {
                    theme::text::error
                } else {
                    theme::text::warning
                })
            }))
            .spacing(15),
    )
    .padding(5)
    .into()
}

#[allow(clippy::too_many_arguments)]
pub fn hw_list_view<'a>(
    i: usize,
    hw: &'a HardwareWallet,
    chosen: bool,
    processing: bool,
    selected: bool,
    descriptor: Option<&'a CoincubeDescriptor>,
    device_must_support_taproot: bool,
    accounts: Option<&HashMap<Fingerprint, ChildNumber>>,
    display_account: bool,
) -> Element<'a, Message> {
    let mut unrelated = false;
    let mut bttn = Button::new(match hw {
        HardwareWallet::Supported {
            kind,
            version,
            fingerprint,
            alias,
            ..
        } => {
            let device_in_descriptor = descriptor
                .map(|d| d.contains_fingerprint(*fingerprint))
                .unwrap_or(true);
            let not_tapminiscript = device_must_support_taproot
                && !is_compatible_with_tapminiscript(kind, version.as_ref());
            if !device_in_descriptor {
                unrelated = true;
                hw::unrelated_hardware_wallet(kind.to_string(), version.as_ref(), fingerprint)
            } else if chosen && processing {
                hw::processing_hardware_wallet(kind, version.as_ref(), fingerprint, alias.as_ref())
            } else if selected {
                let acc = accounts
                    .as_ref()
                    .and_then(|map| map.get(fingerprint).cloned());
                hw::selected_hardware_wallet(
                    kind,
                    version.as_ref(),
                    fingerprint,
                    alias.as_ref(),
                    {
                        if not_tapminiscript {
                            Some("Device firmware version does not support taproot miniscript")
                        } else {
                            None
                        }
                    },
                    acc,
                    display_account,
                )
            } else if not_tapminiscript {
                hw::warning_hardware_wallet(
                    kind,
                    version.as_ref(),
                    fingerprint,
                    alias.as_ref(),
                    "Device firmware version does not support taproot miniscript",
                )
            } else if let Some(accounts) = accounts {
                hw::supported_hardware_wallet_with_account(
                    kind,
                    version.as_ref(),
                    *fingerprint,
                    alias.as_ref(),
                    accounts.get(fingerprint).cloned(),
                    true,
                )
            } else {
                hw::supported_hardware_wallet(kind, version.as_ref(), *fingerprint, alias.as_ref())
            }
        }
        HardwareWallet::Unsupported {
            version,
            kind,
            reason,
            ..
        } => match reason {
            UnsupportedReason::NotPartOfWallet(fg) => {
                hw::unrelated_hardware_wallet(kind.to_string(), version.as_ref(), fg)
            }
            UnsupportedReason::WrongNetwork => {
                hw::wrong_network_hardware_wallet(kind.to_string(), version.as_ref())
            }
            UnsupportedReason::Version {
                minimal_supported_version,
                note,
            } => hw::unsupported_version_hardware_wallet(
                kind.to_string(),
                version.as_ref(),
                minimal_supported_version,
                *note,
            ),
            _ => hw::unsupported_hardware_wallet(kind.to_string(), version.as_ref()),
        },
        HardwareWallet::Locked {
            kind, pairing_code, ..
        } => hw::locked_hardware_wallet(kind, pairing_code.as_ref()),
    })
    .style(theme::button::secondary)
    .width(Length::Fill);
    if !processing && hw.is_supported() && !unrelated {
        bttn = bttn.on_press(Message::Select(i));
    }
    // A firmware advisory rides along under the row. It never gates
    // selection — the device above stays exactly as clickable as it was.
    match crate::hw_advisory::view::hit(hw) {
        Some(hit) => Column::new()
            .push(bttn)
            .push(crate::hw_advisory::view::section(
                &hit,
                hw.fingerprint(),
                Message::OpenUrl(hit.url().to_string()),
                // Dismissal belongs to the app's device list, where the row is
                // long-lived; here the picker is on screen for a moment.
                None,
            ))
            .into(),
        None => bttn.into(),
    }
}

pub fn backup_fresh_fork_mnemonic<'a>(
    progress: (usize, usize),
    email: Option<&'a str>,
    words: String,
    confirmed: bool,
) -> Element<'a, Message> {
    layout(progress, email, "Back Up Your Cube Master Seed",
        Column::new()
            .push(text("Write these words down in order and keep them private. They recover this Cube; the PIN only unlocks this device."))
            .push(text(words))
            .push(iced::widget::checkbox(confirmed).label("I have written down my seed words")
                .on_toggle(Message::ConfirmFreshSeedBackup))
            .push(button::secondary(None, "Next").on_press_maybe(confirmed.then_some(Message::Next)))
            .spacing(30), true, Some(Message::Previous))
}

pub fn backup_mnemonic<'a>(
    progress: (usize, usize),
    email: Option<&'a str>,
) -> Element<'a, Message> {
    layout(
        progress,
        email,
        "Reminder: Back Up Your Master Seed",
        Column::new()
            .push(text(prompt::MNEMONIC_HELP))
            .push(
                button::secondary(None, "Next")
                    .on_press(Message::Next)
                    .width(Length::Fixed(200.0)),
            )
            .push(Space::new().height(20.0))
            .spacing(50),
        true,
        Some(Message::Previous),
    )
}

pub fn recover_mnemonic<'a>(
    progress: (usize, usize),
    email: Option<&'a str>,
    words: &'a [(String, bool); 12],
    current: usize,
    suggestions: &'a [String],
    recover: bool,
    error: Option<&'a String>,
) -> Element<'a, Message> {
    layout(
        progress,
        email,
        "Import Mnemonic",
        Column::new()
            .push(text(prompt::RECOVER_MNEMONIC_HELP))
            .push(if recover {
                Some(
                    Column::new()
                        .align_x(Alignment::Center)
                        .push(
                            Container::new(if !suggestions.is_empty() {
                                suggestions.iter().fold(Row::new().spacing(5), |row, sugg| {
                                    row.push(
                                        Button::new(text(sugg))
                                            .style(theme::button::secondary)
                                            .on_press(Message::MnemonicWord(
                                                current,
                                                sugg.to_string(),
                                            )),
                                    )
                                })
                            } else {
                                Row::new()
                            })
                            // Fixed height in order to not move words list
                            .height(Length::Fixed(50.0)),
                        )
                        .push(words.iter().enumerate().fold(
                            Column::new().spacing(5),
                            |acc, (i, (word, valid))| {
                                acc.push(
                                    Row::new()
                                        .spacing(10)
                                        .align_y(Alignment::Center)
                                        .push(
                                            Container::new(text(format!("#{}", i + 1)).small())
                                                .width(Length::Fixed(50.0)),
                                        )
                                        .push(
                                            Container::new(TextInput::new("", word).on_input(
                                                move |msg| Message::MnemonicWord(i, msg),
                                            ))
                                            .width(Length::Fixed(100.0)),
                                        )
                                        .push(if *valid {
                                            Some(
                                                icon::square_check_icon()
                                                    .style(theme::text::success),
                                            )
                                        } else {
                                            None
                                        }),
                                )
                            },
                        ))
                        .push(Space::new().height(Length::Fixed(50.0)))
                        .push(error.map(|e| card::invalid(text(e).style(theme::text::error)))),
                )
            } else {
                None
            })
            .push(if !recover {
                Row::new()
                    .spacing(10)
                    .push(
                        button::secondary(None, "Import mnemonic")
                            .on_press(Message::ImportMnemonic(true))
                            .width(Length::Fixed(200.0)),
                    )
                    .push(
                        button::secondary(None, "Skip")
                            .on_press(Message::Skip)
                            .width(Length::Fixed(200.0)),
                    )
            } else {
                Row::new()
                    .spacing(10)
                    .push(
                        button::secondary(None, "Cancel")
                            .on_press(Message::ImportMnemonic(false))
                            .width(Length::Fixed(200.0)),
                    )
                    .push(
                        if words.iter().any(|(_, valid)| !valid) || error.is_some() {
                            button::secondary(None, "Next").width(Length::Fixed(200.0))
                        } else {
                            button::secondary(None, "Next")
                                .on_press(Message::Next)
                                .width(Length::Fixed(200.0))
                        },
                    )
            })
            .spacing(50),
        true,
        Some(Message::Previous),
    )
}

pub fn choose_backend(progress: (usize, usize)) -> Element<'static, Message> {
    layout(
        progress,
        None,
        "Choose backend",
        Column::new()
            .push(
                Row::new()
                    .spacing(20)
                    .push(
                        Column::new()
                            .spacing(20)
                            .width(Length::FillPortion(1))
                            .push(h3("Use your own node"))
                            .push(text::p2_medium(LOCAL_WALLET_DESC).style(theme::text::secondary)),
                    )
                    .push(
                        Column::new()
                            .spacing(20)
                            .width(Length::FillPortion(1))
                            .push(h3("Use COINCUBE | Connect"))
                            .push(
                                text::p2_medium(REMOTE_BACKEND_DESC).style(theme::text::secondary),
                            ),
                    ),
            )
            .push(
                Row::new()
                    .spacing(20)
                    .push(
                        Container::new(
                            button::secondary(None, "Select")
                                .on_press(Message::SelectBackend(
                                    message::SelectBackend::ContinueWithLocalWallet(true),
                                ))
                                .width(Length::Fixed(200.0)),
                        )
                        .width(Length::FillPortion(1)),
                    )
                    .push(
                        Container::new(
                            button::secondary(None, "Select")
                                .on_press(Message::SelectBackend(
                                    message::SelectBackend::ContinueWithLocalWallet(false),
                                ))
                                .width(Length::Fixed(200.0)),
                        )
                        .width(Length::FillPortion(1)),
                    ),
            )
            .push(Space::new().height(20)) // ensures mouse cursor is not already on link when arriving at this step
            .push(tooltip::Tooltip::new(
                button::link(
                    Some(icon::link_icon()),
                    "More information about backend and node options",
                )
                .on_press(Message::OpenUrl(
                    help::CHANGE_BACKEND_OR_NODE_URL.to_string(),
                )),
                Container::new(text(help::CHANGE_BACKEND_OR_NODE_URL))
                    .style(theme::card::simple)
                    .padding(10),
                tooltip::Position::Bottom,
            ))
            .spacing(20),
        true,
        Some(Message::Previous),
    )
}

pub fn login(progress: (usize, usize), connection_step: Element<Message>) -> Element<Message> {
    layout(
        progress,
        None,
        "Connect to a shared wallet",
        Container::new(
            Column::new()
                .spacing(20)
                .max_width(700)
                .width(Length::Fill)
                .push(p2_regular("ADD WALLET · CONNECT").style(theme::text::secondary))
                .push(h2("Sign in to Connect"))
                .push(p2_regular(
                    "Use the account associated with this shared wallet. The emailed token is submitted when complete.",
                ))
                .push(connection_step),
        )
        .center_x(Length::Fill),
        true,
        Some(Message::Previous),
    )
}

pub fn connection_step_enter_email<'a>(
    email: &'a form::Value<String>,
    processing: bool,
    connection_error: Option<&'a Error>,
    accounts: &'a [String],
    auth_error: Option<&'a str>,
) -> Element<'a, Message> {
    Column::new()
        .spacing(20)
        .push(if !accounts.is_empty() {
            Some(text("Choose an account you are already using:"))
        } else {
            None
        })
        .push(
            accounts
                .iter()
                .fold(Row::new().spacing(10), |row, a| {
                    row.push(
                        Button::new(Container::new(p1_regular(a)).padding(5))
                            .style(theme::button::secondary)
                            .on_press(Message::SelectBackend(
                                message::SelectBackend::SelectConnectAccount(a.clone()),
                            )),
                    )
                })
                .wrap(),
        )
        .push(connection_error.map(|e| text(e.to_string()).style(theme::text::warning)))
        .push(auth_error.map(|e| text(e.to_string()).style(theme::text::warning)))
        .push(if accounts.is_empty() {
            text("Enter an email you want to associate with the wallet:")
        } else {
            text("Or enter a new email you want to associate with the wallet:")
        })
        .push(
            form::Form::new_trimmed("email", email, |msg| {
                Message::SelectBackend(message::SelectBackend::EmailEdited(msg))
            })
            .size(text::P1_SIZE)
            .padding(10)
            .warning("Email is not valid"),
        )
        .push(
            Row::new().push(Space::new().width(Length::Fill)).push(
                button::secondary(None, "Send token")
                    .on_press_maybe(if processing || !email.valid {
                        None
                    } else {
                        Some(Message::SelectBackend(message::SelectBackend::RequestOTP))
                    })
                    .width(Length::Fixed(200.0)),
            ),
        )
        .into()
}

pub fn connection_step_enter_otp<'a>(
    email: &'a str,
    otp: &form::Value<String>,
    processing: bool,
    connection_error: Option<&Error>,
    auth_error: Option<&'static str>,
) -> Element<'a, Message> {
    Column::new()
        .spacing(20)
        .push(text(email).style(theme::text::success))
        .push(text("An authentication token has been emailed to you"))
        .push(connection_error.map(|e| text(e.to_string()).style(theme::text::warning)))
        .push(auth_error.map(|e| text(e.to_string()).style(theme::text::warning)))
        .push(
            form::Form::new_trimmed("Token", otp, |msg| {
                Message::SelectBackend(message::SelectBackend::OTPEdited(msg))
            })
            .size(text::P1_SIZE)
            .padding(10)
            .warning("Token is not valid"),
        )
        .push(
            Row::new()
                .spacing(10)
                .push(
                    button::secondary(Some(icon::previous_icon()), "Change Email")
                        .on_press(Message::SelectBackend(message::SelectBackend::EditEmail)),
                )
                .push(
                    button::secondary(None, "Resend token").on_press_maybe(if processing {
                        None
                    } else {
                        Some(Message::SelectBackend(message::SelectBackend::RequestOTP))
                    }),
                ),
        )
        .into()
}

pub fn connection_step_connected<'a>(
    email: &'a str,
    processing: bool,
    connection_error: Option<&Error>,
    auth_error: Option<&'static str>,
) -> Element<'a, Message> {
    Column::new()
        .spacing(20)
        .push(text(email).style(theme::text::success))
        .push(connection_error.map(|e| text(e.to_string()).style(theme::text::warning)))
        .push(auth_error.map(|e| text(e.to_string()).style(theme::text::warning)))
        .push(Container::new(
            Row::new()
                .spacing(10)
                .push(
                    button::secondary(Some(icon::previous_icon()), "Change Email")
                        .on_press(Message::SelectBackend(message::SelectBackend::EditEmail)),
                )
                .push(
                    button::secondary(None, "Continue").on_press_maybe(if processing {
                        None
                    } else {
                        Some(Message::Next)
                    }),
                ),
        ))
        .into()
}

pub const REMOTE_BACKEND_DESC: &str = "Use COINCUBE | Connect to instantly be ready to transact. COINCUBE runs the infrastructure, allowing multiple computers or participants to connect and synchronize.\n\nThis is a simpler and safer option for people who want COINCUBE to keep a backup of their descriptor. You are still in control of your keys, and COINCUBE does not have any control over your funds, but it will be able to see your wallet's information, associated to an email address. Privacy focused users should run their own infrastructure instead.";

pub const LOCAL_WALLET_DESC: &str = "Use your already existing Bitcoin node or automatically install one. The Vault wallet will not connect to any external server.\n\nThis is the most private option, but the data is locally stored on this computer, only. You must perform your own backups, and share the descriptor with other people you want to be able to access the wallet.";

/// View for `RestorePinSetupStep`. Two `PinInput` pads stacked
/// vertically, an optional inline error under the confirm pad, and a
/// Next button gated on both pads being complete + matching.
///
/// The `ready` flag is computed in the step (not here) so the view
/// stays dumb — the caller owns the matching rule.
pub fn restore_pin_setup<'a>(
    progress: (usize, usize),
    email: Option<&'a str>,
    entry: &'a crate::pin_input::PinInput,
    confirm: &'a crate::pin_input::PinInput,
    error: Option<&'static str>,
    ready: bool,
) -> Element<'a, Message> {
    use message::{PinField, RestorePinSetupMsg};

    let entry_view = entry
        .view()
        .map(|m| Message::RestorePinSetup(RestorePinSetupMsg::Pin(PinField::Entry, m)));
    let confirm_view = confirm
        .view()
        .map(|m| Message::RestorePinSetup(RestorePinSetupMsg::Pin(PinField::Confirm, m)));

    layout(
        progress,
        email,
        "Create a PIN to secure your Cube",
        Column::new()
            .spacing(40)
            .push(p2_regular(
                "Your Cube's mnemonic will be encrypted with this PIN on disk. \
                 You'll enter it every time you open this Cube — it's how the \
                 Cube decrypts the seed and key material stored on this computer.",
            ))
            .push(
                Column::new()
                    .spacing(15)
                    .push(p1_bold("Enter a 4-digit PIN:"))
                    .push(entry_view),
            )
            .push(
                Column::new()
                    .spacing(15)
                    .push(p1_bold("Confirm PIN:"))
                    .push(confirm_view)
                    .push(error.map(|e| p2_regular(e).style(theme::text::error))),
            )
            .push(
                button::secondary(None, "Next")
                    .width(Length::Fixed(200.0))
                    .on_press_maybe(if ready { Some(Message::Next) } else { None }),
            ),
        true,
        Some(Message::Previous),
    )
}

pub fn wallet_alias<'a>(
    progress: (usize, usize),
    email: Option<&'a str>,
    wallet_alias: &form::Value<String>,
) -> Element<'a, Message> {
    layout(
        progress,
        email,
        "Give your wallet an alias",
        Column::new()
            .push(
                Column::new()
                    .spacing(20)
                    .push(p1_bold("Wallet alias:"))
                    .push(
                        form::Form::new("Wallet alias", wallet_alias, Message::WalletAliasEdited)
                            .warning("Wallet alias is too long.")
                            .size(text::P1_SIZE)
                            .padding(10),
                    )
                    .push(p2_regular(
                        "You will be able to change it later in Settings > Wallet",
                    )),
            )
            .push(
                button::secondary(None, "Next")
                    .width(Length::Fixed(200.0))
                    .on_press_maybe(if wallet_alias.valid {
                        Some(Message::Next)
                    } else {
                        None
                    }),
            )
            .spacing(50),
        true,
        Some(Message::Previous),
    )
}

pub(super) fn layout<'a>(
    progress: (usize, usize),
    email: Option<&'a str>,
    title: &'static str,
    content: impl Into<Element<'a, Message>>,
    padding_left: bool,
    previous_message: Option<Message>,
) -> Element<'a, Message> {
    let mut prev_button =
        button::secondary(Some(icon::previous_icon()), "Back").width(Length::Fixed(150.0));
    if let Some(msg) = previous_message {
        prev_button = prev_button.on_press(msg);
    }
    Container::new(scrollable(
        Column::new()
            .width(Length::Fill)
            .push(
                Row::new()
                    .push(Space::new().width(Length::Fill))
                    .push(email.map(|e| {
                        Container::new(p1_regular(e).style(theme::text::success)).padding(20)
                    })),
            )
            .push(Space::new().height(Length::Fixed(50.0)))
            .push(
                Row::new()
                    .align_y(Alignment::Center)
                    .push(Container::new(prev_button).center_x(Length::FillPortion(2)))
                    .push(Container::new(h3(title)).width(Length::FillPortion(8)))
                    .push(if progress.1 > 0 {
                        Some(
                            Container::new(text(format!("{} | {}", progress.0, progress.1)))
                                .center_x(Length::FillPortion(2)),
                        )
                    } else {
                        None
                    }),
            )
            .push(
                Row::new()
                    .push(Space::new().width(Length::FillPortion(2)))
                    .push(
                        Container::new(
                            Column::new()
                                .push(Space::new().height(Length::Fixed(40.0)))
                                .push(content),
                        )
                        .width(Length::FillPortion(if padding_left {
                            8
                        } else {
                            10
                        })),
                    )
                    .push(if padding_left {
                        Some(Space::new().width(Length::FillPortion(2)))
                    } else {
                        None
                    }),
            ),
    ))
    .center_x(Length::Fill)
    .height(Length::Fill)
    .width(Length::Fill)
    .style(theme::container::background)
    .into()
}

/// The Recovery Kit journey keeps each existing authentication, selection and
/// decryption phase visible. The step's state machine remains authoritative.
#[allow(clippy::too_many_arguments)]
pub fn recovery_kit_restore<'a>(
    progress: (usize, usize),
    scope: crate::installer::step::RestoreScope,
    phase: &'a crate::installer::step::recovery_kit_restore::RestorePhase,
    email: &'a form::Value<String>,
    otp: &'a form::Value<String>,
    password: &'a str,
    processing: bool,
    error: Option<&'a str>,
) -> Element<'a, Message> {
    use crate::installer::message::RecoveryKitRestoreMsg;
    use crate::installer::step::recovery_kit_restore::RestorePhase;

    let title = match scope {
        crate::installer::step::RestoreScope::Full => "Restore from Cube Recovery Kit",
        crate::installer::step::RestoreScope::DescriptorOnly => {
            "Restore Wallet Descriptor from Connect"
        }
    };

    let (step_label, step_title, step_detail) = match phase {
        RestorePhase::Email | RestorePhase::OtpEntry => (
            "RECOVERY KIT · STEP 1 OF 3",
            "Sign in to Connect",
            "Verify the account that holds this Cube's Recovery Kit.",
        ),
        RestorePhase::LoadingCubes | RestorePhase::CubePicker { .. } => (
            "RECOVERY KIT · STEP 2 OF 3",
            "Choose a Cube",
            match scope {
                crate::installer::step::RestoreScope::Full => "Cubes on this network are listed. Only kits with a Master Seed can be selected for Cube recovery.",
                crate::installer::step::RestoreScope::DescriptorOnly => "Cubes on this network are listed. Only kits with a wallet descriptor can be selected.",
            },
        ),
        RestorePhase::PasswordEntry { .. } | RestorePhase::Decrypting { .. } => (
            "RECOVERY KIT · STEP 3 OF 3",
            "Open the Recovery Kit",
            "The recovery password decrypts the backup on this device.",
        ),
        RestorePhase::Ready { .. } => (
            "RECOVERY KIT · READY",
            "Continue restoring",
            "Your Recovery Kit was decrypted. Continue with the remaining installer steps.",
        ),
        RestorePhase::Error { .. } => (
            "RECOVERY KIT · NEEDS ATTENTION",
            "Recovery needs attention",
            "Review the error below, then retry the existing restore flow.",
        ),
    };
    let mut content: Column<'a, Message> = Column::new()
        .spacing(20)
        .width(Length::Fill)
        .max_width(700)
        .push(p2_regular(step_label).style(theme::text::secondary))
        .push(h4_bold(step_title))
        .push(p2_regular(step_detail).style(theme::text::secondary));

    match phase {
        RestorePhase::Email => {
            content = content
                .push(text::text(
                    "Sign in to your Connect account to fetch your Cube Recovery Kit.",
                ))
                .push(
                    form::Form::new_trimmed("you@example.com", email, |v| {
                        Message::RecoveryKitRestore(RecoveryKitRestoreMsg::EmailEdited(v))
                    })
                    .warning("Email is invalid")
                    .size(16)
                    .padding(10),
                )
                .push({
                    let btn = button::primary(None, "Send code")
                        .width(Length::Fixed(220.0))
                        .padding([10, 20]);
                    if email.valid && !processing {
                        btn.on_press(Message::RecoveryKitRestore(
                            RecoveryKitRestoreMsg::RequestOtp,
                        ))
                    } else {
                        btn
                    }
                });
        }
        RestorePhase::OtpEntry => {
            content = content
                .push(text::text(format!(
                    "Enter the 6-digit code sent to {}.",
                    email.value
                )))
                .push(
                    form::Form::new_trimmed("123456", otp, |v| {
                        // Wrap immediately — the `String` from iced's
                        // input callback is the last unprotected copy
                        // in our code path; from here it lives inside
                        // `Zeroizing` all the way to the async task.
                        Message::RecoveryKitRestore(RecoveryKitRestoreMsg::OtpEdited(
                            zeroize::Zeroizing::new(v),
                        ))
                    })
                    .warning("Invalid code")
                    .size(16)
                    .padding(10),
                );
        }
        RestorePhase::LoadingCubes => {
            content = content.push(text::text("Loading your Cubes from Connect…"));
        }
        RestorePhase::CubePicker { cubes, .. } => {
            content = content.push(p2_regular("Recovery Kit availability"));
            let mut col = Column::new().spacing(10);
            if cubes.is_empty() {
                col = col.push(text::text(
                    "No Cubes found on this Connect account for the current network.",
                ));
            } else {
                for c in cubes {
                    // Scope-aware availability: a descriptor-only
                    // kit can't satisfy a Full restore, and a
                    // seed-only kit can't satisfy DescriptorOnly.
                    // Disable the row (and say why) rather than
                    // let the user click through to a post-decrypt
                    // "missing half" error.
                    let available = c.status.has_recovery_kit
                        && match scope {
                            crate::installer::step::RestoreScope::Full => {
                                c.status.has_encrypted_seed
                            }
                            crate::installer::step::RestoreScope::DescriptorOnly => {
                                c.status.has_encrypted_wallet_descriptor
                            }
                        };
                    let label = if available {
                        format!("{} · {} · Recovery Kit available", c.name, c.network)
                    } else if !c.status.has_recovery_kit {
                        format!("{} · {} · No Recovery Kit", c.name, c.network)
                    } else {
                        match scope {
                            crate::installer::step::RestoreScope::Full => {
                                format!("{} · {} · No Master Seed in kit", c.name, c.network)
                            }
                            crate::installer::step::RestoreScope::DescriptorOnly => {
                                format!("{} · {} · No descriptor in kit", c.name, c.network)
                            }
                        }
                    };
                    let id = c.id;
                    // Raw `iced::widget::Button` because the labels are
                    // built from dynamic `String`s — `button::secondary`
                    // requires `&'static str`.
                    let mut btn = Button::new(text::text(label))
                        .width(Length::Fill)
                        .padding([8, 16])
                        .style(theme::button::secondary);
                    if available {
                        btn = btn.on_press(Message::RecoveryKitRestore(
                            RecoveryKitRestoreMsg::SelectCube(id),
                        ));
                    }
                    col = col.push(btn);
                }
            }
            content = content.push(col);
        }
        RestorePhase::PasswordEntry { selected, .. } => {
            content = content
                .push(text::text(format!(
                    "Enter the recovery password for \"{}\".",
                    selected.name
                )))
                .push(
                    TextInput::new("Recovery password", password)
                        .on_input(|v| {
                            // Wrap the password in `Zeroizing` before
                            // it enters the message queue — see
                            // `RecoveryKitRestoreMsg` doc. The `String`
                            // from iced's callback is the last plain
                            // copy; every subsequent clone is
                            // auto-zeroed on drop.
                            Message::RecoveryKitRestore(RecoveryKitRestoreMsg::PasswordEdited(
                                zeroize::Zeroizing::new(v),
                            ))
                        })
                        .secure(true)
                        .size(16)
                        .padding(12)
                        .width(Length::Fixed(400.0)),
                )
                .push({
                    let btn = button::primary(None, "Restore")
                        .width(Length::Fixed(220.0))
                        .padding([10, 20]);
                    if !password.is_empty() && !processing {
                        btn.on_press(Message::RecoveryKitRestore(
                            RecoveryKitRestoreMsg::SubmitPassword,
                        ))
                    } else {
                        btn
                    }
                });
        }
        RestorePhase::Decrypting { .. } => {
            content = content.push(text::text(
                "Decrypting your Recovery Kit on this device. This can take a moment.",
            ));
        }
        RestorePhase::Ready { selected, .. } => {
            // The happy path after a successful decrypt fires
            // `Message::Next` automatically, so the user rarely
            // lingers on this screen. But a user can land here
            // again by navigating back through the installer —
            // without an explicit Next button they'd be stuck.
            content = content
                .push(text::text(format!(
                    "Recovery Kit for \"{}\" decrypted. Continue to the remaining installer steps.",
                    selected.name
                )))
                .push(
                    button::primary(None, "Next")
                        .on_press(Message::Next)
                        .width(Length::Fixed(220.0))
                        .padding([10, 20]),
                );
        }
        RestorePhase::Error { message } => {
            content = content
                .push(text::text(message.clone()).style(theme::text::warning))
                .push(
                    button::secondary(None, "Try again")
                        .on_press(Message::RecoveryKitRestore(
                            RecoveryKitRestoreMsg::RetryFromStart,
                        ))
                        .width(Length::Fixed(220.0))
                        .padding([10, 20]),
                );
        }
    }

    if let Some(e) = error {
        content = content.push(text::text(e.to_string()).style(theme::text::warning));
    }

    // Skip button — only for DescriptorOnly scopes.
    if matches!(scope, crate::installer::step::RestoreScope::DescriptorOnly) {
        content = content.push(
            button::secondary(None, "Skip — restore later")
                .on_press(Message::RecoveryKitRestore(RecoveryKitRestoreMsg::Skip))
                .width(Length::Fixed(220.0))
                .padding([10, 20]),
        );
    }

    layout(
        progress,
        Some(email.value.as_str())
            .filter(|value| !value.is_empty() && !matches!(phase, RestorePhase::Email)),
        title,
        content,
        true,
        Some(Message::Previous),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::{
        installer::step::{
            recovery_kit_restore::{RestoreCubeCandidate, RestorePhase},
            RestoreScope,
        },
        node::bitcoind::NodeFlavor,
        services::coincube::{RecoveryKitStatus, RECOVERY_KIT_SCHEME_AES_256_GCM},
    };

    fn value(text: &str, valid: bool) -> form::Value<String> {
        form::Value {
            value: text.to_string(),
            warning: None,
            valid,
        }
    }

    fn recovery_status(
        has_recovery_kit: bool,
        has_encrypted_seed: bool,
        has_encrypted_wallet_descriptor: bool,
    ) -> RecoveryKitStatus {
        RecoveryKitStatus {
            has_recovery_kit,
            has_encrypted_seed,
            has_encrypted_wallet_descriptor,
            encryption_scheme: RECOVERY_KIT_SCHEME_AES_256_GCM.to_string(),
            created_at: None,
            updated_at: None,
            owner_self: None,
        }
    }

    fn candidate(
        id: u64,
        name: &str,
        has_recovery_kit: bool,
        has_encrypted_seed: bool,
        has_encrypted_wallet_descriptor: bool,
    ) -> RestoreCubeCandidate {
        RestoreCubeCandidate {
            id,
            uuid: format!("cube-{id}"),
            name: name.to_string(),
            network: "bitcoin".to_string(),
            status: recovery_status(
                has_recovery_kit,
                has_encrypted_seed,
                has_encrypted_wallet_descriptor,
            ),
        }
    }

    #[test]
    fn expire_message_units_formats_minutes_hours_days_months_and_years() {
        assert!(expire_message_units(0).is_empty());
        assert_eq!(expire_message_units(1), vec!["10m".to_string()]);
        assert_eq!(expire_message_units(6), vec!["1h".to_string()]);
        assert_eq!(
            expire_message_units(144),
            vec!["1d".to_string()],
            "144 ten-minute blocks is roughly one day"
        );
        assert_eq!(expire_message_units(4383), vec!["1m".to_string()]);
        assert_eq!(expire_message_units(52596), vec!["1y".to_string()]);
        assert_eq!(
            expire_message_units(52596 + 4383 + 144),
            vec!["1y".to_string(), "1m".to_string(), "1d".to_string()]
        );
    }

    #[test]
    fn node_and_login_views_construct_for_main_branches() {
        let valid = value("127.0.0.1:8332", true);
        let remote = value("192.168.1.10:8332", true);
        let invalid = value("not an address", false);
        let auth = RpcAuthValues {
            cookie_path: value("/tmp/.cookie", true),
            user: value("rpcuser", true),
            password: value("rpcpass", true),
        };

        let _ = define_bitcoind(&valid, &auth, &RpcAuthType::CookieFile);
        let _ = define_bitcoind(&remote, &auth, &RpcAuthType::UserPass);
        let _ = define_electrum(&valid, true);
        let _ = define_esplora(&invalid, "https://example.test/api");

        let _ = choose_backend((1, 3));
        let _ = connection_step_enter_email(
            &value("user@example.com", true),
            false,
            None,
            &["user@example.com".to_string()],
            None,
        );
        let otp = value("123456", true);
        let _ = connection_step_enter_otp("user@example.com", &otp, false, None, None);
        let _ = connection_step_connected("user@example.com", false, None, None);
        let _ = login(
            (2, 3),
            connection_step_connected("user@example.com", true, None, None),
        );
    }

    #[test]
    fn connect_and_bitcoind_choice_views_construct_for_authenticated_and_local_paths() {
        let email = value("user@example.com", true);
        let otp = value("123456", true);
        let prune = value("15000", true);
        let max_mempool = value("", true);

        let _ = define_coincube_connect((1, 4), &email, &otp, false, true, false, None, true);
        let _ = define_coincube_connect(
            (1, 4),
            &email,
            &otp,
            true,
            false,
            true,
            Some("try again"),
            false,
        );

        let _ = select_bitcoind_type(
            (2, 4),
            1,
            crate::chain::ChainId::Regtest,
            false,
            false,
            15_000,
            true,
            NodeFlavor::Core,
            None,
            true,
        );
        let _ = select_bitcoind_type(
            (2, 4),
            2,
            crate::chain::ChainId::Bitcoin,
            true,
            true,
            15_000,
            true,
            NodeFlavor::Knots,
            None,
            true,
        );
        let _ = select_bitcoind_type(
            (2, 4),
            3,
            crate::chain::ChainId::Bitcoin,
            false,
            false,
            15_000,
            false,
            NodeFlavor::Core,
            None,
            true,
        );

        let _ = start_internal_bitcoind(
            (3, 4),
            10,
            NodeFlavor::Knots,
            Some(NodeFlavor::Core),
            false,
            true,
            &prune,
            &max_mempool,
            None,
            None,
            None,
            Some(&DownloadState::Idle),
            None,
        );
        let _ = start_internal_bitcoind(
            (3, 4),
            11,
            NodeFlavor::Core,
            None,
            true,
            false,
            &prune,
            &max_mempool,
            Some(&PathBuf::from("/tmp/bitcoind")),
            Some(&Ok(())),
            None,
            Some(&DownloadState::Finished(vec![])),
            Some(&InstallState::Finished),
        );
    }

    #[test]
    fn simple_installer_views_construct_for_ready_and_blocked_states() {
        let alias = value("My vault", true);
        let invalid_alias = value("This alias is too long", false);
        let warn = "Seed backup still recommended".to_string();

        let _ = install((4, 4), Some("user@example.com"), true, false, None, None);
        let _ = install(
            (4, 4),
            None,
            false,
            true,
            Some(&warn),
            Some("Connect backup enabled".to_string()),
        );
        let _ = wallet_alias((1, 2), None, &alias);
        let _ = wallet_alias((1, 2), Some("user@example.com"), &invalid_alias);
    }

    #[test]
    fn recovery_kit_restore_views_construct_for_each_phase() {
        let email = value("user@example.com", true);
        let otp = value("123456", true);
        let available = candidate(1, "Available", true, true, true);
        let no_kit = candidate(2, "No Kit", false, false, false);
        let descriptor_only = candidate(3, "Descriptor Only", true, false, true);

        let phases = vec![
            RestorePhase::Email,
            RestorePhase::OtpEntry,
            RestorePhase::LoadingCubes,
            RestorePhase::CubePicker {
                cubes: vec![available.clone(), no_kit, descriptor_only.clone()],
                selected: None,
            },
            RestorePhase::PasswordEntry {
                selected: available.clone(),
                attempts: 1,
            },
            RestorePhase::Decrypting {
                selected: available.clone(),
            },
            RestorePhase::Ready {
                selected: descriptor_only,
                seed: None,
                descriptor: None,
            },
            RestorePhase::Error {
                message: "could not decrypt".to_string(),
            },
        ];

        for phase in &phases {
            let _ = recovery_kit_restore(
                (1, 5),
                RestoreScope::DescriptorOnly,
                phase,
                &email,
                &otp,
                "correct horse battery staple",
                false,
                Some("inline error"),
            );
        }

        let _ = recovery_kit_restore(
            (1, 5),
            RestoreScope::Full,
            &RestorePhase::CubePicker {
                cubes: vec![candidate(4, "Seed Only", true, true, false)],
                selected: None,
            },
            &email,
            &otp,
            "",
            true,
            None,
        );
    }
}

#[cfg(test)]
mod policy_layout_tests {
    use super::*;
    use coincube_core::miniscript::bitcoin::{
        bip32::{ChainCode, Xpub},
        secp256k1::PublicKey,
        NetworkKind,
    };
    use iced::advanced::{
        layout,
        renderer::Headless,
        widget::{Id, Operation, Tree},
        Layout,
    };

    #[derive(Default)]
    struct MountedPolicy {
        labels: Vec<(String, iced::Rectangle)>,
        containers: Vec<iced::Rectangle>,
        scrolls: Vec<(iced::Rectangle, iced::Rectangle)>,
    }

    impl Operation for MountedPolicy {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn text(&mut self, _: Option<&Id>, bounds: iced::Rectangle, value: &str) {
            self.labels.push((value.to_owned(), bounds));
        }

        fn container(&mut self, _: Option<&Id>, bounds: iced::Rectangle) {
            self.containers.push(bounds);
        }

        fn scrollable(
            &mut self,
            _: Option<&Id>,
            bounds: iced::Rectangle,
            content_bounds: iced::Rectangle,
            _: iced::Vector,
            _: &mut dyn iced::advanced::widget::operation::Scrollable,
        ) {
            self.scrolls.push((bounds, content_bounds));
        }
    }

    fn public_xpub(chain_byte: u8) -> Xpub {
        Xpub {
            network: NetworkKind::Test,
            depth: 3,
            parent_fingerprint: [1, 2, 3, 4].into(),
            child_number: bitcoin::bip32::ChildNumber::Hardened { index: 0 },
            public_key: PublicKey::from_str(
                "0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798",
            )
            .unwrap(),
            chain_code: ChainCode::from([chain_byte; 32]),
        }
    }

    fn policy_fixture() -> (
        CoincubeDescriptor,
        HashMap<Fingerprint, settings::KeySetting>,
    ) {
        let first = public_xpub(3);
        let second = public_xpub(5);
        let third = public_xpub(7);
        let descriptor = CoincubeDescriptor::from_str(&format!(
            "wsh(andor(pk([01020304/48'/1'/0'/2']{first}/<0;1>/*),older(144),andor(pk([05060708/48'/1'/0'/2']{second}/<0;1>/*),older(52596),pk([090a0b0c/48'/1'/0'/2']{third}/<0;1>/*))))"
        ))
        .unwrap();
        let keys = IntoIterator::into_iter([
            (
                [1, 2, 3, 4],
                "Synthetic first recovery signer with a long display name",
            ),
            (
                [5, 6, 7, 8],
                "Synthetic second recovery signer with a long display name",
            ),
            (
                [9, 10, 11, 12],
                "Synthetic primary signer with a long display name",
            ),
        ])
        .map(|(bytes, name)| {
            let fingerprint = Fingerprint::from(bytes);
            (
                fingerprint,
                settings::KeySetting {
                    name: name.to_owned(),
                    master_fingerprint: fingerprint,
                    provider_key: None,
                    is_border_wallet: false,
                    grid_seed_source: None,
                    replay_protected: None,
                    keychain_key_id: None,
                },
            )
        })
        .collect();
        (descriptor, keys)
    }

    #[tokio::test]
    async fn mounted_installer_backup_shell_keeps_recovery_copy_inside_card() {
        for bytes in coincube_ui::font::load() {
            iced::advanced::graphics::text::font_system()
                .write()
                .unwrap()
                .load_font(bytes);
        }
        let (descriptor, keys) = policy_fixture();
        assert_eq!(descriptor.policy().recovery_paths().len(), 2);
        let capture_dir = std::env::var("R11A_INSTALLER_CAPTURE_DIR").ok();
        if let Some(directory) = &capture_dir {
            std::fs::create_dir_all(directory).unwrap();
        }
        for (width, height) in [(1024, 768), (1440, 900), (1024, 2400), (1440, 2400)] {
            for (theme_name, theme) in [
                ("dark", theme::Theme::dark()),
                ("light", theme::Theme::light()),
            ] {
                let mut renderer = <iced::Renderer as Headless>::new(
                    coincube_ui::font::REGULAR,
                    iced::Pixels(16.0),
                    Some("tiny-skia"),
                )
                .await
                .expect("software renderer");
                let mut element = backup_descriptor((0, 0), None, &descriptor, &keys, None, false);
                let mut tree = Tree::new(element.as_widget());
                let size = iced::Size::new(width as f32, height as f32);
                let node = element.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(iced::Size::ZERO, size),
                );
                let mut mounted = MountedPolicy::default();
                element.as_widget_mut().operate(
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &mut mounted,
                );
                for required in [
                    "The wallet policy:",
                    "can always spend this wallet's funds (Primary path)",
                    "(Recovery path #1)",
                    "(Recovery path #2)",
                    "Synthetic first recovery signer with a long display name",
                    "Synthetic second recovery signer with a long display name",
                    "Synthetic primary signer with a long display name",
                ] {
                    assert!(
                        mounted.labels.iter().any(|(text, _)| text == required),
                        "{}px {}: missing complete policy text: {}",
                        width,
                        theme_name,
                        required
                    );
                }
                assert_eq!(
                    mounted
                        .labels
                        .iter()
                        .filter(|(text, _)| text == "can spend coins after a relative timelock of")
                        .count(),
                    2,
                    "{}px {}: both recovery clauses must mount",
                    width,
                    theme_name
                );
                for sequence in ["144 blocks (~1d)", "52596 blocks (~1y)"] {
                    assert!(
                        mounted.labels.iter().any(|(text, _)| text == sequence),
                        "{}px {}: missing recovery block delay: {}",
                        width,
                        theme_name,
                        sequence
                    );
                }
                let heading = mounted
                    .labels
                    .iter()
                    .find(|(text, _)| text == "The wallet policy:")
                    .expect("mounted policy heading")
                    .1;
                let primary_clause = mounted
                    .labels
                    .iter()
                    .find(|(text, _)| text == "can always spend this wallet's funds (Primary path)")
                    .expect("mounted primary policy clause")
                    .1;
                let (viewport, content) = mounted
                    .scrolls
                    .iter()
                    .filter(|(_, content)| {
                        primary_clause.x >= content.x
                            && primary_clause.x < content.x + content.width
                            && primary_clause.y >= content.y
                            && primary_clause.y < content.y + content.height
                    })
                    .min_by(|(left, _), (right, _)| left.width.total_cmp(&right.width))
                    .expect("mounted horizontal policy scroll viewport");
                assert!(
                    viewport.x >= -1.0 && viewport.x + viewport.width <= width as f32 + 1.0,
                    "{}px {}: policy viewport escapes window: {:?}",
                    width,
                    theme_name,
                    viewport
                );
                assert!(
                    viewport.x > width as f32 * 0.1
                        && viewport.x + viewport.width < width as f32 * 0.9,
                    "{}px {}: policy viewport did not follow the installer shell width: {:?}",
                    width,
                    theme_name,
                    viewport
                );
                let card = mounted
                    .containers
                    .iter()
                    .filter(|card| {
                        heading.x >= card.x - 1.0
                            && heading.x + heading.width <= card.x + card.width + 1.0
                            && heading.y >= card.y - 1.0
                            && heading.y + heading.height <= card.y + card.height + 1.0
                            && viewport.x >= card.x - 1.0
                            && viewport.x + viewport.width <= card.x + card.width + 1.0
                            && viewport.y >= card.y - 1.0
                            && viewport.y + viewport.height <= card.y + card.height + 1.0
                    })
                    .min_by(|left, right| {
                        (left.width * left.height).total_cmp(&(right.width * right.height))
                    })
                    .expect("smallest card containing policy heading and scroll viewport");
                assert!(
                    card.width <= width as f32 * 0.8,
                    "{}px {}: policy card escapes installer content column: {:?}",
                    width,
                    theme_name,
                    card
                );
                let start = mounted
                    .labels
                    .iter()
                    .position(|(text, _)| {
                        text == "can always spend this wallet's funds (Primary path)"
                    })
                    .unwrap();
                let end = mounted
                    .labels
                    .iter()
                    .position(|(text, _)| text == "I have backed up my descriptor")
                    .unwrap();
                for (text, bounds) in &mounted.labels[start..end] {
                    assert!(
                        bounds.x >= content.x - 1.0
                            && bounds.x + bounds.width <= content.x + content.width + 1.0,
                        "{}px {}: policy text escapes scroll content: {} {:?} {:?}",
                        width,
                        theme_name,
                        text,
                        bounds,
                        content
                    );
                }
                let screen = iced::Rectangle::with_size(size);
                element.as_widget().draw(
                    &tree,
                    &mut renderer,
                    &theme,
                    &iced::advanced::renderer::Style {
                        text_color: theme.colors.text.primary,
                    },
                    Layout::new(&node),
                    iced::mouse::Cursor::Unavailable,
                    &screen,
                );
                if let Some(directory) = &capture_dir {
                    let pixels = renderer.screenshot(
                        iced::Size::new(width, height),
                        1.0,
                        theme.colors.general.background,
                    );
                    image::save_buffer(
                        std::path::Path::new(directory).join(format!(
                            "r11a-installer-policy-{}-{}x{}.png",
                            theme_name, width, height
                        )),
                        &pixels,
                        width,
                        height,
                        image::ColorType::Rgba8,
                    )
                    .unwrap();
                }
            }
        }
    }
}

#[cfg(test)]
mod node_management_chain_tests {
    use super::*;
    use crate::{chain::ChainId, node::bitcoind::NodeFlavor};
    use iced::advanced::{
        layout,
        renderer::Headless,
        widget::{operation, operation::focusable, operation::Focusable, Id, Operation, Tree},
        Layout,
    };
    use iced::{keyboard, mouse, Event, Rectangle, Size};

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

    #[derive(Default)]
    struct FocusedActions(Vec<(Id, bool)>);

    impl Operation for FocusedActions {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn focusable(&mut self, id: Option<&Id>, _: Rectangle, state: &mut dyn Focusable) {
            if let Some(id) = id {
                self.0.push((id.clone(), state.is_focused()));
            }
        }
    }

    #[test]
    fn internal_node_flavor_focus_is_scoped_to_one_installer_pane() {
        let prune = form::Value {
            value: "15000".to_owned(),
            valid: true,
            warning: None,
        };
        let mempool = form::Value::default();
        let mut element: Element<'_, Message> = Row::new()
            .push(start_internal_bitcoind(
                (3, 4),
                41,
                NodeFlavor::Knots,
                None,
                false,
                false,
                &prune,
                &mempool,
                None,
                None,
                None,
                None,
                None,
            ))
            .push(start_internal_bitcoind(
                (3, 4),
                42,
                NodeFlavor::Knots,
                None,
                false,
                false,
                &prune,
                &mempool,
                None,
                None,
                None,
                None,
                None,
            ))
            .into();
        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("software renderer");
        let size = Size::new(2048.0, 1200.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let left = Id::from("tenshu.installer.node.41.flavor.core".to_owned());
        let right = Id::from("tenshu.installer.node.42.flavor.core".to_owned());
        let mut focus: Box<dyn Operation<()>> = Box::new(focusable::focus::<()>(right.clone()));
        loop {
            element.as_widget_mut().operate(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &mut operation::black_box(focus.as_mut()),
            );
            match focus.finish() {
                operation::Outcome::None | operation::Outcome::Some(()) => break,
                operation::Outcome::Chain(next) => focus = next,
            }
        }
        let mut focused = FocusedActions::default();
        element
            .as_widget_mut()
            .operate(&mut tree, Layout::new(&node), &renderer, &mut focused);
        assert_eq!(
            focused
                .0
                .iter()
                .find(|(id, _)| id == &left)
                .map(|(_, on)| *on),
            Some(false),
            "the first pane's Core action must stay unfocused"
        );
        assert_eq!(
            focused
                .0
                .iter()
                .find(|(id, _)| id == &right)
                .map(|(_, on)| *on),
            Some(true),
            "the second pane's Core action must receive focus"
        );
    }

    fn activate_node_action(
        mut element: Element<'static, Message>,
        target: &str,
        key: keyboard::Key,
        focus_with_tab: bool,
    ) -> Vec<Message> {
        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("software renderer");
        let size = Size::new(1024.0, 1200.0);
        let mut tree = Tree::new(element.as_widget());
        let node = element.as_widget_mut().layout(
            &mut tree,
            &renderer,
            &layout::Limits::new(Size::ZERO, size),
        );
        let mut focus: Box<dyn Operation<()>> = if focus_with_tab {
            Box::new(focusable::focus_next::<()>())
        } else {
            Box::new(focusable::focus::<()>(Id::from(target.to_owned())))
        };
        loop {
            element.as_widget_mut().operate(
                &mut tree,
                Layout::new(&node),
                &renderer,
                &mut operation::black_box(focus.as_mut()),
            );
            match focus.finish() {
                operation::Outcome::None | operation::Outcome::Some(()) => break,
                operation::Outcome::Chain(next) => focus = next,
            }
        }
        if focus_with_tab {
            let mut focused = FocusedActions::default();
            element
                .as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut focused);
            assert!(
                focused
                    .0
                    .iter()
                    .any(|(id, on)| id == &Id::from(target.to_owned()) && *on),
                "Tab navigation did not focus {target}: {:?}",
                focused.0
            );
        }
        let physical_key =
            keyboard::key::Physical::Unidentified(keyboard::key::NativeCode::Unidentified);
        let mut emitted = Vec::new();
        for event in [
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key.clone(),
                physical_key,
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
                text: None,
                repeat: false,
            }),
            Event::Keyboard(keyboard::Event::KeyReleased {
                key: key.clone(),
                modified_key: key,
                physical_key,
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::empty(),
            }),
        ] {
            element.as_widget_mut().update(
                &mut tree,
                &event,
                Layout::new(&node),
                mouse::Cursor::Unavailable,
                &renderer,
                &mut iced::advanced::clipboard::Null,
                &mut iced::advanced::Shell::new(&mut emitted),
                &Rectangle::with_size(size),
            );
        }
        emitted
    }

    #[test]
    fn node_install_action_names_both_states_and_activates_after_tab() {
        let renderer = iced::futures::executor::block_on(<iced::Renderer as Headless>::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        ))
        .expect("software renderer");

        for label in [
            "Also install a pruned node on my device",
            "Also install a pruned Bitcoin node on my device",
        ] {
            for checked in [false, true] {
                let mut element = node_install_toggle(42, checked, label);
                let mut tree = Tree::new(element.as_widget());
                let node = element.as_widget_mut().layout(
                    &mut tree,
                    &renderer,
                    &layout::Limits::new(Size::ZERO, Size::new(620.0, 80.0)),
                );
                let mut labels = Labels::default();
                element.as_widget_mut().operate(
                    &mut tree,
                    Layout::new(&node),
                    &renderer,
                    &mut labels,
                );
                let (mark, state_and_action) = if checked {
                    ("☑", "On — activate to turn Off")
                } else {
                    ("☐", "Off — activate to turn On")
                };
                assert!(
                    labels
                        .0
                        .iter()
                        .any(|value| value == &format!("{} {}: {}", mark, label, state_and_action)),
                    "focusable action must name its current state and next action: {:?}",
                    labels.0
                );
                for key in [keyboard::key::Named::Enter, keyboard::key::Named::Space] {
                    let messages = activate_node_action(
                        node_install_toggle(42, checked, label),
                        "tenshu.installer.node.42.install",
                        keyboard::Key::Named(key),
                        true,
                    );
                    assert!(
                        matches!(
                            messages.as_slice(),
                            [Message::SelectBitcoindType(
                                message::SelectBitcoindTypeMsg::ToggleInstallNode
                            )]
                        ),
                        "checked={} label={} key={:?}: {:?}",
                        checked,
                        label,
                        key,
                        messages
                    );
                }
            }
        }
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn node_choices_activate_with_enter_and_space() {
        use keyboard::key::Named;
        let cases: &[(ChainId, bool, bool, bool, &str, fn(&Message) -> bool)] = &[
            (ChainId::Regtest, false, false, false, "existing", |m| {
                matches!(
                    m,
                    Message::SelectBitcoindType(message::SelectBitcoindTypeMsg::UseExternal(true))
                )
            }),
            (ChainId::Bitcoin, false, false, false, "local", |m| {
                matches!(
                    m,
                    Message::SelectBitcoindType(message::SelectBitcoindTypeMsg::UseExternal(false))
                )
            }),
            (ChainId::Bitcoin, true, false, false, "continue", |m| {
                matches!(
                    m,
                    Message::SelectBitcoindType(
                        message::SelectBitcoindTypeMsg::ContinueWithConnect
                    )
                )
            }),
            (ChainId::Bitcoin, true, false, false, "advanced", |m| {
                matches!(
                    m,
                    Message::SelectBitcoindType(message::SelectBitcoindTypeMsg::ToggleAdvanced)
                )
            }),
            (
                ChainId::Bitcoin,
                true,
                true,
                false,
                "advanced-connect",
                |m| {
                    matches!(
                        m,
                        Message::SelectBitcoindType(message::SelectBitcoindTypeMsg::UseConnect)
                    )
                },
            ),
            (ChainId::Bitcoin, true, false, true, "flavor.core", |m| {
                matches!(
                    m,
                    Message::SelectBitcoindType(message::SelectBitcoindTypeMsg::SelectFlavor(
                        NodeFlavor::Core
                    ))
                )
            }),
            (
                ChainId::BitcoinBlake2b,
                true,
                false,
                true,
                "blake2b-connect",
                |m| {
                    matches!(
                        m,
                        Message::SelectBitcoindType(
                            message::SelectBitcoindTypeMsg::ContinueWithConnect
                        )
                    )
                },
            ),
            (
                ChainId::BitcoinBlake2b,
                true,
                false,
                true,
                "blake2b-local",
                |m| {
                    matches!(
                        m,
                        Message::SelectBitcoindType(message::SelectBitcoindTypeMsg::UseExternal(
                            false
                        ))
                    )
                },
            ),
        ];
        for (chain, authenticated, advanced, install, suffix, expected) in cases {
            for key in [Named::Enter, Named::Space] {
                let element = select_bitcoind_type(
                    (2, 5),
                    42,
                    *chain,
                    *install,
                    *advanced,
                    15_000,
                    *authenticated,
                    if chain.is_blake2b() {
                        NodeFlavor::KnotsBlake2b
                    } else {
                        NodeFlavor::Knots
                    },
                    None,
                    true,
                );
                let target = format!("tenshu.installer.node.42.{}", suffix);
                let messages =
                    activate_node_action(element, &target, keyboard::Key::Named(key), false);
                assert_eq!(messages.len(), 1, "{target} {key:?}: {messages:?}");
                assert!(
                    expected(&messages[0]),
                    "{} {:?}: {:?}",
                    target,
                    key,
                    messages
                );
            }
        }
    }

    #[tokio::test]
    async fn node_management_copy_and_choices_follow_exact_chain() {
        use crate::chain::ChainIdExt;
        let renderer = <iced::Renderer as Headless>::new(
            iced::Font::DEFAULT,
            iced::Pixels(16.0),
            Some("tiny-skia"),
        )
        .await
        .expect("software renderer");
        for chain in [
            ChainId::Bitcoin,
            ChainId::BitcoinBlake2b,
            ChainId::BitcoinBlake2bTestnet4,
        ] {
            let mut element = select_bitcoind_type(
                (0, 2),
                4,
                chain,
                true,
                true,
                15000,
                true,
                if chain.is_blake2b() {
                    NodeFlavor::KnotsBlake2b
                } else {
                    NodeFlavor::Knots
                },
                None,
                true,
            );
            let mut tree = Tree::new(element.as_widget());
            let node = element.as_widget_mut().layout(
                &mut tree,
                &renderer,
                &layout::Limits::new(iced::Size::ZERO, iced::Size::new(1600.0, 2200.0)),
            );
            let mut labels = Labels::default();
            element
                .as_widget_mut()
                .operate(&mut tree, Layout::new(&node), &renderer, &mut labels);
            if chain.is_blake2b() {
                assert!(labels.0.iter().any(|s| s == "Use a local node"));
                assert!(labels
                    .0
                    .iter()
                    .any(|s| s.contains("No Connect account is required")
                        && s.contains(chain.label())));
                assert!(labels.0.iter().any(|s| s == "Use Connect"));
                assert!(!labels.0.iter().any(|s| s == "I already have a node"));
            } else {
                assert!(labels.0.iter().any(|s| s == "Install a node only"));
                assert!(labels.0.iter().any(|s| s == "Bitcoin Knots"));
                assert!(labels.0.iter().any(|s| s == "Bitcoin Core"));
                assert!(!labels
                    .0
                    .iter()
                    .any(|s| s.contains("not available in this version")));
            }
        }
    }
}
