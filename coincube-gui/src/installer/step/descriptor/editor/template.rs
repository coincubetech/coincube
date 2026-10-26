use iced::Task;
use std::sync::atomic::{AtomicU64, Ordering};

use coincube_ui::widget::Element;

use crate::{
    hw::HardwareWallets,
    installer::{
        context::DescriptorTemplate,
        message::Message,
        step::{Context, Step},
        view,
    },
};

static NEXT_TEMPLATE_UI_INSTANCE: AtomicU64 = AtomicU64::new(1);

fn next_template_ui_instance() -> u64 {
    NEXT_TEMPLATE_UI_INSTANCE.fetch_add(1, Ordering::Relaxed)
}

pub struct ChooseDescriptorTemplate {
    template: DescriptorTemplate,
    ui_instance: u64,
}

impl Default for ChooseDescriptorTemplate {
    fn default() -> Self {
        Self {
            template: DescriptorTemplate::default(),
            ui_instance: next_template_ui_instance(),
        }
    }
}

impl From<ChooseDescriptorTemplate> for Box<dyn Step> {
    fn from(s: ChooseDescriptorTemplate) -> Box<dyn Step> {
        Box::new(s)
    }
}
impl Step for ChooseDescriptorTemplate {
    fn update(&mut self, _hws: &mut HardwareWallets, message: Message) -> Task<Message> {
        if let Message::SelectDescriptorTemplate(template) = message {
            self.template = template;
            Task::perform(async move {}, |_| Message::Next)
        } else {
            Task::none()
        }
    }

    fn apply(&mut self, ctx: &mut Context) -> bool {
        ctx.descriptor_template = self.template;
        true
    }

    fn view<'a>(
        &'a self,
        _hws: &'a HardwareWallets,
        progress: (usize, usize),
        _email: Option<&'a str>,
    ) -> Element<'a, Message> {
        view::editor::template::choose_descriptor_template(progress, self.ui_instance)
    }
}

pub struct DescriptorTemplateDescription {
    template: DescriptorTemplate,
    show_keychain_tip: bool,
    ui_instance: u64,
}

impl Default for DescriptorTemplateDescription {
    fn default() -> Self {
        Self {
            template: DescriptorTemplate::default(),
            show_keychain_tip: false,
            ui_instance: next_template_ui_instance(),
        }
    }
}

impl From<DescriptorTemplateDescription> for Box<dyn Step> {
    fn from(s: DescriptorTemplateDescription) -> Box<dyn Step> {
        Box::new(s)
    }
}

impl Step for DescriptorTemplateDescription {
    fn load_context(&mut self, ctx: &Context) {
        self.template = ctx.descriptor_template;
        self.show_keychain_tip = !ctx.bitcoin_config.chain.is_blake2b()
            && ctx.cube_id.is_some()
            && ctx.coincube_client.is_some();
    }

    fn view<'a>(
        &'a self,
        _hws: &'a HardwareWallets,
        progress: (usize, usize),
        _email: Option<&'a str>,
    ) -> Element<'a, Message> {
        match self.template {
            DescriptorTemplate::SimpleInheritance => {
                view::editor::template::inheritance::inheritance_template_description(
                    progress,
                    self.ui_instance,
                    self.show_keychain_tip,
                )
            }
            DescriptorTemplate::MultisigSecurity => {
                view::editor::template::multisig_security_wallet::multisig_security_template_description(
                    progress,
                    self.ui_instance,
                    self.show_keychain_tip,
                )
            }
            DescriptorTemplate::Custom => {
                view::editor::template::custom::custom_template_description(
                    progress,
                    self.ui_instance,
                    self.show_keychain_tip,
                )
            }
            DescriptorTemplate::TwoOfThreeInheritance => {
                view::editor::template::inheritance::two_of_three_inheritance_description(
                    progress,
                    self.ui_instance,
                    self.show_keychain_tip,
                )
            }
            DescriptorTemplate::MultisigInheritanceRecovery => {
                view::editor::template::multisig_security_wallet::multisig_inheritance_recovery_description(
                    progress,
                    self.ui_instance,
                    self.show_keychain_tip,
                )
            }
            DescriptorTemplate::ExpandingMultisigInheritanceRecovery => {
                view::editor::template::multisig_security_wallet::expanding_multisig_inheritance_recovery_description(
                    progress,
                    self.ui_instance,
                    self.show_keychain_tip,
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separate_installer_steps_have_stable_distinct_action_scopes() {
        let first = ChooseDescriptorTemplate::default();
        let second = ChooseDescriptorTemplate::default();
        let intro = DescriptorTemplateDescription::default();
        assert_ne!(first.ui_instance, second.ui_instance);
        assert_ne!(first.ui_instance, intro.ui_instance);
    }
}
