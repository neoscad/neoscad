// "Connect your AI agent" in the window (docs/windows-app.md, "AI agents";
// the Windows part of docs/audits/agent-connection-desktop.md's UI spec):
//
// - the control at the right end of the menu row: "Connect your AI agent",
//   then "Claude Code connected" with a dot, or a busy ring and "Claude
//   Code is editing" while it works. Before the user allows agents it
//   opens the dialog; after, a flyout with who is connected, Disconnect
//   for each, and "Ask before applying edits". Hidden once the user turns
//   agents off; the Help menu is then the way back;
// - Help > Connect Your AI Agent…: the dialog. Its first switch is the
//   consent, then a selector bar of clients (Claude Code first, the last
//   pick kept) over the chosen client's card with its one button
//   (NeoSCAD.Host/AgentSetup.cs), then "Using NeoSCAD with your agent",
//   an Expander with the core's text for that client;
// - Help > Allow AI Agents… and Ask Before Applying Agent Edits: the same
//   two settings, as the Help menu already holds the update settings (the
//   app has no settings window);
// - the approval bar, when the user asked to be asked.
//
// The link and the document's side are NeoSCAD.Host's AgentConnection and
// AgentDocumentHost; this file is controls only.

using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using NeoSCAD.Host;
using NeoSCAD.Native;
using Windows.ApplicationModel.DataTransfer;

namespace NeoSCAD.App;

public sealed partial class MainWindow
{
    const string AgentsHelpUrl = "https://neoscad.org/agents.html";

    const string ConsentText =
        "Agents on this computer that use NeoSCAD (Claude Code, Cursor, VS Code and others) will be able to " +
        "read and edit the models open in NeoSCAD and see the 3D view. What they read goes to the agent's AI service.";

    static readonly string AgentSettingsPath =
        AgentSettings.PathIn(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData));

    AgentConnection? agents;
    Flyout? agentFlyout;
    /// <summary>The edit the approval bar is asking about.</summary>
    TaskCompletionSource<bool>? pendingApproval;

    /// <summary>From the constructor, once the editor and the view exist.</summary>
    void StartAgents()
    {
        string version;
        try
        {
            version = NeoScad.CoreVersion();
        }
        catch (Exception e) when (e is CoreException or DllNotFoundException)
        {
            // No core, no link: nothing for an agent to work with here.
            AgentButton.Visibility = Visibility.Collapsed;
            return;
        }
        agents = new AgentConnection(document, new WinUiDispatcher(DispatcherQueue), editor, () => view.Viewport,
            version, AgentSettingsPath)
        {
            Approve = AskAboutEditAsync,
        };
        agents.Changed += ShowAgentState;
        Activated += (_, e) => agents?.Activated(e.WindowActivationState != WindowActivationState.Deactivated);
        ShowAgentState();
    }

    /// <summary>
    /// The window is closing: a question still up is answered no (the
    /// agent hears "declined" instead of waiting out its deadline), then
    /// the link goes before the document does.
    /// </summary>
    void StopAgents()
    {
        DecideEdit(false);
        agents?.Dispose();
        agents = null;
    }

    void ShowAgentState()
    {
        if (agents is null) return;
        var settings = agents.Settings;
        AllowAgentsItem.IsChecked = settings.Allowed;
        AskAgentEditsItem.IsChecked = settings.AskBeforeEdits;
        AskAgentEditsItem.IsEnabled = settings.Allowed;
        var indicator = agents.Indicator;
        AgentButton.Visibility = indicator.State == AgentIndicatorState.Hidden ? Visibility.Collapsed : Visibility.Visible;
        AgentLabel.Text = indicator.Text;
        ToolTipService.SetToolTip(AgentButton, indicator.Tooltip);
        AgentDot.Visibility = indicator.State == AgentIndicatorState.Connected ? Visibility.Visible : Visibility.Collapsed;
        var working = indicator.State == AgentIndicatorState.Working;
        AgentBusy.IsActive = working;
        AgentBusy.Visibility = working ? Visibility.Visible : Visibility.Collapsed;
        if (agentFlyout is { IsOpen: true } f) f.Content = AgentFlyoutContent();
        if (!settings.Allowed || !settings.AskBeforeEdits) DecideEdit(false);
    }

    // --- The control and its flyout ---------------------------------------------------

    async void OnAgentButton(object sender, RoutedEventArgs e)
    {
        if (agents is null) return;
        if (!agents.Settings.Allowed)
        {
            await ShowConnectDialogAsync();
            return;
        }
        agentFlyout = new Flyout { Content = AgentFlyoutContent(), Placement = FlyoutPlacementMode.BottomEdgeAlignedRight };
        agentFlyout.ShowAt(AgentButton);
    }

    UIElement AgentFlyoutContent()
    {
        var panel = new StackPanel { Spacing = 10, MinWidth = 300, MaxWidth = 380 };
        if (agents is null) return panel;
        panel.Children.Add(new TextBlock
        {
            Text = agents.Indicator.Text,
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
        });
        var clients = agents.Status?.Clients ?? [];
        if (clients.Length == 0)
        {
            panel.Children.Add(Secondary(agents.Status?.Error is { } error
                ? $"NeoSCAD couldn't listen for agents: {error}"
                : "No agent is connected. Add NeoSCAD to your agent, then ask it about this model."));
        }
        foreach (var client in clients)
        {
            var row = new Grid { ColumnSpacing = 8 };
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            // The name is what the agent calls itself, not a checked identity.
            var name = client.Name ?? "An agent";
            row.Children.Add(new TextBlock
            {
                Text = client.Activity is { } activity ? $"{name} {activity}" : $"{name}, connected",
                TextWrapping = TextWrapping.Wrap,
                VerticalAlignment = VerticalAlignment.Center,
            });
            var disconnect = new Button { Content = "Disconnect" };
            var id = client.Id;
            disconnect.Click += (_, _) => agents?.Disconnect(id);
            Grid.SetColumn(disconnect, 1);
            row.Children.Add(disconnect);
            panel.Children.Add(row);
        }
        var ask = new ToggleSwitch
        {
            Header = "Ask me before applying the agent's edits",
            IsOn = agents.Settings.AskBeforeEdits,
        };
        ask.Toggled += (_, _) => agents?.SetAskBeforeEdits(ask.IsOn);
        panel.Children.Add(ask);
        var links = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
        var setup = new HyperlinkButton { Content = "Add NeoSCAD to an agent…" };
        setup.Click += async (_, _) =>
        {
            agentFlyout?.Hide();
            await ShowConnectDialogAsync();
        };
        var off = new HyperlinkButton { Content = "Turn off AI agents" };
        off.Click += (_, _) =>
        {
            agentFlyout?.Hide();
            agents?.SetAllowed(false);
        };
        links.Children.Add(setup);
        links.Children.Add(off);
        panel.Children.Add(links);
        return panel;
    }

    static TextBlock Secondary(string text) => new()
    {
        Text = text,
        TextWrapping = TextWrapping.Wrap,
        Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
    };

    // --- The Help menu ------------------------------------------------------------------

    async void OnConnectAgent(object sender, RoutedEventArgs e) => await ShowConnectDialogAsync();

    async void OnAgentHelp(object sender, RoutedEventArgs e) =>
        await Windows.System.Launcher.LaunchUriAsync(new Uri(AgentsHelpUrl));

    async void OnAllowAgentsToggle(object sender, RoutedEventArgs e)
    {
        if (agents is null) return;
        if (!AllowAgentsItem.IsChecked)
        {
            agents.SetAllowed(false);
            return;
        }
        // The item checks itself on click; it stays checked only with a yes.
        AllowAgentsItem.IsChecked = false;
        if (await AskConsentAsync()) agents.SetAllowed(true);
        ShowAgentState();
    }

    void OnAskAgentEditsToggle(object sender, RoutedEventArgs e) => agents?.SetAskBeforeEdits(AskAgentEditsItem.IsChecked);

    /// <summary>The consent question (the audit's words), on its own: from the Help menu's switch.</summary>
    async Task<bool> AskConsentAsync()
    {
        if (dialogShowing) return false;
        var dialog = new ContentDialog
        {
            XamlRoot = Root.XamlRoot,
            Title = "Let AI agents work on your open models?",
            Content = new TextBlock { Text = ConsentText, TextWrapping = TextWrapping.Wrap },
            PrimaryButtonText = "Allow",
            CloseButtonText = "Not Now",
            DefaultButton = ContentDialogButton.Primary,
        };
        dialogShowing = true;
        try
        {
            return await dialog.ShowAsync() == ContentDialogResult.Primary;
        }
        finally
        {
            dialogShowing = false;
        }
    }

    // --- The dialog ---------------------------------------------------------------------

    async Task ShowConnectDialogAsync()
    {
        if (agents is null || dialogShowing) return;
        var cli = AgentCli.Locate(AppContext.BaseDirectory, Environment.GetEnvironmentVariable("PATH"), windows: true);
        var setup = new AgentSetup(NativeAgentSetup.Instance, cli,
            async url => await Windows.System.Launcher.LaunchUriAsync(new Uri(url)));
        var content = new StackPanel { Spacing = 16 };

        // 1. The consent, as a switch with its explanation beside it.
        var allow = new ToggleSwitch
        {
            Header = "Allow AI agents to work on open documents",
            IsOn = agents.Settings.Allowed,
        };
        var state = Secondary(AgentDialogState());
        allow.Toggled += (_, _) => agents?.SetAllowed(allow.IsOn);
        Action refresh = () =>
        {
            if (agents is null) return;
            if (allow.IsOn != agents.Settings.Allowed) allow.IsOn = agents.Settings.Allowed;
            state.Text = AgentDialogState();
        };
        agents.Changed += refresh;
        var access = new StackPanel { Spacing = 4 };
        access.Children.Add(allow);
        access.Children.Add(Secondary(ConsentText));
        access.Children.Add(state);
        content.Children.Add(access);

        // 2. The client picker, and the chosen client's card under it.
        content.Children.Add(new TextBlock
        {
            Text = "Add NeoSCAD to your agent",
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
        });
        if (cli is not null) content.Children.Add(Secondary($"Each agent runs NeoSCAD's own command-line tool, {cli}."));
        var picker = new SelectorBar();
        foreach (var row in setup.Rows)
        {
            var item = new SelectorBarItem { Text = setup.ShortLabel(row.Client), Tag = row.Client };
            AutomationProperties.SetName(item, row.Row.Label);
            picker.Items.Add(item);
            // Selected before the handler is attached, so opening the
            // dialog doesn't count as the user picking.
            if (row.Client == setup.Selected) picker.SelectedItem = item;
        }
        var card = new Border
        {
            Padding = new Thickness(12),
            CornerRadius = new CornerRadius(6),
            Background = (Brush)Application.Current.Resources["CardBackgroundFillColorDefaultBrush"],
            BorderBrush = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"],
            BorderThickness = new Thickness(1),
        };
        // Rebuilt on every change and every pick, so a card's "Copied"
        // never carries over to the next client; a row's own state (a
        // Replace or a Claude Desktop question waiting) stays in its model
        // and is still there on coming back to it.
        void ShowCard() => card.Child = setup.SelectedRow is { } r ? SetupRow(setup, r) : null;
        ShowCard();
        setup.Changed += row =>
        {
            if (row == setup.SelectedRow) ShowCard();
        };
        var setupPanel = new StackPanel { Spacing = 8 };
        setupPanel.Children.Add(picker);
        setupPanel.Children.Add(card);
        content.Children.Add(setupPanel);

        // 3. How to work with it, for the chosen client: the core's text.
        var usage = new Expander
        {
            Header = new TextBlock
            {
                Text = "Using NeoSCAD with your agent",
                Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
            },
            IsExpanded = agents.Settings.UsageOpen,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            Content = UsagePanel(setup),
        };
        usage.Expanding += (_, _) => agents?.SetUsageOpen(true);
        usage.Collapsed += (_, _) => agents?.SetUsageOpen(false);
        content.Children.Add(usage);

        picker.SelectionChanged += (_, _) =>
        {
            if (picker.SelectedItem?.Tag is not AgentSetupClient client || !setup.Select(client)) return;
            agents?.SetSetupClient(client);
            ShowCard();
            usage.Content = UsagePanel(setup);
        };

        var dialog = new ContentDialog
        {
            XamlRoot = Root.XamlRoot,
            Title = "Connect your AI agent",
            Content = new ScrollViewer { Content = content, Padding = new Thickness(0, 0, 16, 0) },
            CloseButtonText = "Done",
            DefaultButton = ContentDialogButton.Close,
        };
        // Wider than a ContentDialog's default 548, so a row's command fits.
        dialog.Resources["ContentDialogMaxWidth"] = 720.0;
        _ = setup.DetectAsync();
        dialogShowing = true;
        try
        {
            await dialog.ShowAsync();
        }
        finally
        {
            dialogShowing = false;
            agents.Changed -= refresh;
        }
    }

    string AgentDialogState()
    {
        if (agents is null) return "";
        if (!agents.Settings.Allowed) return "Off: agents can still work on saved files, but not on this window.";
        var clients = agents.Status?.Clients ?? [];
        if (agents.Status?.Error is { } error) return $"NeoSCAD couldn't listen for agents: {error}";
        return clients.Length == 0
            ? "On. No agent is connected yet: add NeoSCAD to one below, then ask it about this model."
            : $"On. {agents.Indicator.Text}.";
    }

    /// <summary>
    /// "Using NeoSCAD with your agent": a headed item per topic, a sentence
    /// or two each, the example requests under "Things to ask", and the
    /// web page for more. Selectable, so a user can copy an example into
    /// their agent.
    /// </summary>
    static UIElement UsagePanel(AgentSetup setup)
    {
        var panel = new StackPanel { Spacing = 12 };
        foreach (var entry in setup.Usage())
        {
            var item = new Grid { ColumnSpacing = 10 };
            item.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(20) });
            item.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            var icon = new FontIcon
            {
                Glyph = UsageGlyph(entry.Topic),
                FontSize = 16,
                VerticalAlignment = VerticalAlignment.Top,
                Margin = new Thickness(0, 2, 0, 0),
                Foreground = (Brush)Application.Current.Resources["AccentTextFillColorPrimaryBrush"],
            };
            AutomationProperties.SetAccessibilityView(icon, AccessibilityView.Raw);
            item.Children.Add(icon);
            var text = new StackPanel { Spacing = 2 };
            text.Children.Add(new TextBlock { Text = entry.Title, FontWeight = FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap });
            var body = Secondary(entry.Body);
            body.IsTextSelectionEnabled = true;
            text.Children.Add(body);
            foreach (var example in entry.Examples)
            {
                text.Children.Add(new TextBlock
                {
                    Text = example,
                    FontStyle = Windows.UI.Text.FontStyle.Italic,
                    TextWrapping = TextWrapping.Wrap,
                    IsTextSelectionEnabled = true,
                });
            }
            Grid.SetColumn(text, 1);
            item.Children.Add(text);
            panel.Children.Add(item);
        }
        panel.Children.Add(new HyperlinkButton
        {
            Content = "Learn more",
            NavigateUri = new Uri(AgentsHelpUrl),
            Margin = new Thickness(18, 0, 0, 0),
        });
        return panel;
    }

    /// <summary>Segoe Fluent Icons for each topic, as the macOS sheet gives each an SF Symbol.</summary>
    static string UsageGlyph(AgentUsageTopic topic) => topic switch
    {
        AgentUsageTopic.KeepOpen => "\uE7F4", // TVMonitor (a screen)
        AgentUsageTopic.WhatToAsk => "\uE8BD", // Message
        AgentUsageTopic.Edits => "\uE7A7", // Undo
        AgentUsageTopic.Seeing => "\uE890", // View (an eye)
        AgentUsageTopic.Control => "\uE72E", // Lock
        AgentUsageTopic.Export => "\uEDE1", // Export
        _ => "\uE946", // Info
    };

    UIElement SetupRow(AgentSetup setup, AgentSetupRowModel row)
    {
        var grid = new Grid { ColumnSpacing = 8, RowSpacing = 4 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var text = new StackPanel { Spacing = 2 };
        text.Children.Add(new TextBlock { Text = row.Row.Label, FontWeight = FontWeights.SemiBold });
        var status = row.Status.Length > 0 ? row.Status : row.Row.Note;
        if (status.Length > 0) text.Children.Add(Secondary(status));
        if (row.Detail is { } detail)
        {
            text.Children.Add(new TextBlock
            {
                Text = detail,
                TextWrapping = TextWrapping.Wrap,
                IsTextSelectionEnabled = true,
                Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
            });
        }
        // What to copy is shown when copying is the way: nothing to run,
        // or running failed.
        if (row.Step is SetupStep.Unavailable or SetupStep.Failed || row.Row.Action is AgentSetupAction.CopyOnly)
        {
            text.Children.Add(new TextBlock
            {
                Text = row.Row.CopyText,
                TextWrapping = TextWrapping.Wrap,
                IsTextSelectionEnabled = true,
                FontFamily = new FontFamily("Cascadia Mono, Consolas"),
                FontSize = 12,
            });
        }
        grid.Children.Add(text);

        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6, VerticalAlignment = VerticalAlignment.Top };
        if (row.ButtonText is { } label)
        {
            var main = new Button { Content = label, IsEnabled = row.ButtonEnabled };
            if (row.Step == SetupStep.Confirm) main.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
            main.Click += async (_, _) => await setup.RunAsync(row);
            buttons.Children.Add(main);
            if (row.Step == SetupStep.Confirm)
            {
                var cancel = new Button { Content = "Cancel" };
                cancel.Click += (_, _) => setup.Cancel(row);
                buttons.Children.Add(cancel);
            }
        }
        var copy = new Button { Content = "Copy" };
        ToolTipService.SetToolTip(copy, row.Row.CopyText);
        copy.Click += (_, _) =>
        {
            var package = new DataPackage();
            package.SetText(row.Row.CopyText);
            Clipboard.SetContent(package);
            copy.Content = "Copied";
        };
        buttons.Children.Add(copy);
        Grid.SetColumn(buttons, 1);
        grid.Children.Add(buttons);
        return grid;
    }

    // --- Ask before applying ---------------------------------------------------------------

    /// <summary>
    /// The approval bar for one edit (on the UI thread): true for Apply.
    /// Another edit asking replaces it (the first is declined), and the
    /// agent's deadline takes the bar away as declined.
    /// </summary>
    Task<bool> AskAboutEditAsync(AgentEditRequest edit, CancellationToken cancel)
    {
        DecideEdit(false);
        var decision = new TaskCompletionSource<bool>();
        pendingApproval = decision;
        // The name is the agent's own claim.
        AgentEditBar.Title = $"{edit.Client ?? "An agent"} wants to change this model";
        AgentEditBar.Message = edit.Summary.Length > 0
            ? $"{edit.Summary}. It will be highlighted, and Undo takes it back."
            : "It will be highlighted, and Undo takes it back.";
        AgentEditBar.IsOpen = true;
        cancel.Register(() => DispatcherQueue.TryEnqueue(() =>
        {
            if (pendingApproval == decision) DecideEdit(false);
        }));
        return decision.Task;
    }

    void DecideEdit(bool apply)
    {
        var pending = pendingApproval;
        pendingApproval = null;
        if (pending is null) return;
        AgentEditBar.IsOpen = false;
        pending.TrySetResult(apply);
    }

    void OnAgentEditApply(object sender, RoutedEventArgs e) => DecideEdit(true);

    void OnAgentEditReject(InfoBar sender, object args) => DecideEdit(false);
}
