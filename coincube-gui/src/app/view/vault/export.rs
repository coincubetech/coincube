use coincube_ui::locale::copy as localized_copy;
use coincube_ui::{
    component::{
        button, card, hw,
        text::{h4_bold, text},
    },
    icon,
    widget::Element,
};
use iced::{
    alignment::{self, Horizontal},
    widget::{progress_bar, Column, Container, Row, Space},
    Length,
};

use crate::export::ImportExportState;
use crate::export::{Error, ImportExportMessage, ImportExportType};

fn interrupted_message(state: &ImportExportState, operation: &ImportExportType) -> &'static str {
    let importing = matches!(
        operation,
        ImportExportType::ImportBackup { .. }
            | ImportExportType::FromBackup
            | ImportExportType::ImportPsbt(_)
            | ImportExportType::ImportXpub(_)
            | ImportExportType::ImportDescriptor
    );
    match (state, importing) {
        (ImportExportState::TimedOut, true) => {
            "Import timed out. Check whether anything was imported before trying again."
        }
        (ImportExportState::Aborted, true) => {
            "Import canceled. Check whether anything was imported before trying again."
        }
        (ImportExportState::TimedOut, false) => {
            "Export timed out. Check the chosen destination before trying again; the file may be incomplete."
        }
        (ImportExportState::Aborted, false) => {
            "Export canceled. Check the chosen destination before trying again."
        }
        _ => unreachable!("only timeout and cancellation use this copy"),
    }
}

/// An import/export error in the window's language. `Error`'s Display is
/// also logged, so it stays English; Spanish is composed here, passing any
/// inner diagnostic through `notice`.
fn shown_error(error: &Error) -> String {
    use coincube_ui::locale::{language, notice, Language};
    if language() == Language::English {
        return error.to_string();
    }
    let inner = |detail: String| notice(detail);
    match error {
        Error::Io(e) => format!("Error de E/S al importar o exportar: {}", inner(e.clone())),
        Error::HandleLost => "Se perdió el proceso de importación o exportación.".into(),
        Error::UnexpectedEnd => {
            "El proceso de importación o exportación terminó de forma inesperada.".into()
        }
        Error::JoinError(e) => format!(
            "No se pudo completar la importación o exportación: {}",
            inner(e.clone())
        ),
        Error::ChannelLost => "Se cerró el canal de importación o exportación.".into(),
        Error::NoParentDir => "La ubicación elegida no tiene una carpeta superior.".into(),
        Error::Daemon(e) => format!(
            "Error del daemon al importar o exportar: {}",
            inner(e.clone())
        ),
        Error::UnsafeLegacyAlternative => notice(error.to_string()),
        Error::TxTimeMissing => "Falta la altura de bloque de una transacción.".into(),
        Error::DaemonMissing => "No hay un daemon disponible para importar o exportar.".into(),
        Error::ParsePsbt => "No se pudo leer el PSBT.".into(),
        Error::ParseDescriptor => "No se pudo leer el descriptor.".into(),
        Error::Bip329Export(e) => format!(
            "No se pudieron exportar las etiquetas BIP-329: {}",
            inner(e.clone())
        ),
        Error::BackupImport(e) => format!("No se pudo importar la copia: {}", inner(e.clone())),
        Error::Backup(e) => format!("Error en la copia: {}", inner(e.to_string())),
        Error::EncryptedBackup(e) => format!("No se pudo cifrar la copia: {:?}", e),
        Error::EncryptionFailed => "Falló el cifrado.".into(),
        Error::ParseXpub => "No se pudo leer el xpub del archivo.".into(),
        Error::XpubNetwork => "El xpub es de otra red.".into(),
        Error::TxidNotMatch => "El txid del PSBT importado no coincide con este PSBT.".into(),
        Error::InsanePsbt => "El PSBT no es válido.".into(),
        Error::OutpointNotOwned => {
            "La importación falló. El PSBT no pertenece a la cartera o ya se gastó.".into()
        }
        Error::UnknownFormat => "Formato de archivo desconocido.".into(),
    }
}

/// Return the modal view for an export task
pub fn export_modal<'a, Message: From<ImportExportMessage> + Clone + 'static>(
    state: &ImportExportState,
    error: Option<&'a Error>,
    title: &'a str,
    import_export_type: &ImportExportType,
    advisory: Option<&'static crate::hw_advisory::Advisory>,
) -> Element<'a, Message> {
    let cancel = match state {
        ImportExportState::Started | ImportExportState::Progress(_) => Some(
            button::secondary(None, localized_copy("Cancel"))
                .on_press(ImportExportMessage::UserStop.into()),
        ),
        _ => None,
    }
    .map(Container::new);

    let cross = match state {
        ImportExportState::Ended | ImportExportState::TimedOut | ImportExportState::Aborted => {
            Some(
                button::transparent(Some(icon::cross_icon().size(30)), "")
                    .on_press(ImportExportMessage::Close.into()),
            )
        }
        _ => None,
    };

    let msg = if let Some(error) = error {
        shown_error(error)
    } else {
        match state {
            ImportExportState::Init => "".to_string(),
            ImportExportState::ChoosePath => {
                localized_copy("Choose a location in the system file dialog…").into()
            }
            ImportExportState::Path(_) => "".into(),
            ImportExportState::Started => localized_copy("Starting the import or export…").into(),
            ImportExportState::Progress(p) => {
                coincube_ui::localized_format!("Progress: {}%", "Progreso: {}%", p.round())
            }
            ImportExportState::TimedOut | ImportExportState::Aborted => {
                localized_copy(interrupted_message(state, import_export_type)).into()
            }
            ImportExportState::Ended => localized_copy(import_export_type.end_message()).into(),
            ImportExportState::Closed => "".into(),
        }
    };
    let labels_btn = (
        localized_copy("Imported labels conflict with labels on this device. Overwrite replaces local labels; Ignore keeps them.").to_string(),
        Some(Container::new(
            Row::new()
                .push(
                    button::secondary(None, localized_copy("Overwrite"))
                        .on_press(ImportExportMessage::Overwrite.into()),
                )
                .push(Space::new().width(30))
                .push(
                    button::secondary(None, localized_copy("Ignore")).on_press(ImportExportMessage::Ignore.into()),
                ),
        )),
    );
    let aliases_btn = (
        localized_copy("Imported signer aliases conflict with names on this device. Overwrite replaces local names; Ignore keeps them.").to_string(),
        Some(Container::new(
            Row::new()
                .push(
                    button::secondary(None, localized_copy("Overwrite"))
                        .on_press(ImportExportMessage::Overwrite.into()),
                )
                .push(Space::new().width(30))
                .push(
                    button::secondary(None, localized_copy("Ignore")).on_press(ImportExportMessage::Ignore.into()),
                ),
        )),
    );
    let (msg, button) = match import_export_type {
        ImportExportType::ImportBackup {
            overwrite_labels,
            overwrite_aliases,
            ..
        } => match (overwrite_labels, overwrite_aliases) {
            (Some(_), _) => labels_btn,

            (_, Some(_)) => aliases_btn,
            _ => (msg, cancel),
        },
        _ => (msg, cancel),
    };
    let button = button.map(|b| {
        Container::new(b)
            .align_x(Horizontal::Center)
            .width(Length::Fill)
    });

    // Only measured progress is shown as a percentage. A canceled or timed-out
    // operation must never look complete merely because its dialog has ended.
    let progress_bar_row = match state {
        ImportExportState::Progress(p) => Some(
            Row::new()
                .push(Space::new().width(30))
                .push(progress_bar(0.0..=100.0, *p))
                .push(Space::new().width(30)),
        ),
        _ => None,
    };

    // Firmware advisory for the signer this file came from. Informational
    // only: the import has already succeeded by the time it appears.
    let has_advisory = advisory.is_some();
    let advisory = advisory.map(|advisory| {
        Row::new()
            .push(Space::new().width(20))
            .push(hw::advisory_panel(
                advisory.headline,
                None,
                advisory.file_import,
                advisory.guide_label,
                Some(ImportExportMessage::OpenAdvisoryUrl(advisory.url.to_string()).into()),
                None,
            ))
            .push(Space::new().width(20))
    });
    card::simple(
        Column::new()
            .spacing(10)
            .push(
                Row::new()
                    .push(Space::new().width(20))
                    .push(h4_bold(localized_copy(title)))
                    .push(Space::new().width(Length::Fill))
                    .push(cross)
                    .align_y(alignment::Vertical::Center),
            )
            .push(Space::new().height(Length::Fill))
            .push(progress_bar_row)
            .push(Space::new().height(Length::Fill))
            .push(Row::new().push(text(msg)))
            .push(advisory)
            .push(Space::new().height(Length::Fill))
            .push(button)
            .push(Space::new().height(5)),
    )
    .width(Length::Fixed(500.0))
    // The fixed height is what every other import/export modal uses; an
    // advisory adds a paragraph, so that one case sizes to its content.
    .height(if has_advisory {
        Length::Shrink
    } else {
        Length::Fixed(300.0)
    })
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_import_and_export_copy_identifies_the_right_result_to_check() {
        for state in [ImportExportState::TimedOut, ImportExportState::Aborted] {
            let import = interrupted_message(&state, &ImportExportType::ImportDescriptor);
            assert!(import.contains("Import"));
            assert!(import.contains("imported"));
            assert!(!import.contains("destination"));

            let export = interrupted_message(&state, &ImportExportType::Transactions);
            assert!(export.contains("Export"));
            assert!(export.contains("destination"));
        }
    }

    #[test]
    fn export_window_is_spanish_in_every_state() {
        use crate::app::view::shared::landing::capture;
        use coincube_ui::locale::{render_in, Language};
        use coincube_ui::theme::Theme;

        let inspect =
            |scenario: &str,
             state: ImportExportState,
             error: Option<Error>,
             kind: ImportExportType,
             advisory: Option<&'static crate::hw_advisory::Advisory>| {
                let view = render_in(Language::Spanish, || {
                    export_modal::<crate::app::view::Message>(
                        &state,
                        error.as_ref(),
                        "Export Transactions",
                        &kind,
                        advisory,
                    )
                });
                capture::inspect("export-es", view, (1440, 900), "dark", Theme::dark())
                    .assert_spanish(scenario);
            };

        let states = || {
            [
                ImportExportState::ChoosePath,
                ImportExportState::Started,
                ImportExportState::Progress(40.0),
                ImportExportState::TimedOut,
                ImportExportState::Aborted,
                ImportExportState::Ended,
            ]
        };
        for kind in [
            ImportExportType::Transactions,
            ImportExportType::ImportDescriptor,
        ] {
            for state in states() {
                let scenario = format!("{:?} {:?}", state, kind);
                inspect(&scenario, state, None, kind.clone(), None);
            }
        }
        for error in [
            Error::XpubNetwork,
            Error::OutpointNotOwned,
            Error::Io("denied".into()),
        ] {
            let scenario = format!("error {:?}", error);
            inspect(
                &scenario,
                ImportExportState::Ended,
                Some(error),
                ImportExportType::ImportDescriptor,
                None,
            );
        }
        for advisory in crate::hw_advisory::ADVISORIES {
            inspect(
                advisory.headline,
                ImportExportState::Ended,
                None,
                ImportExportType::ImportXpub(coincube_core::miniscript::bitcoin::Network::Bitcoin),
                Some(advisory),
            );
        }
    }
}
