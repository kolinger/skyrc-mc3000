App = function () {
};

App.prototype.bind = function () {
    var self = this;

    var adjustScrollHeight = function () {
        var height = $(window).height();
        var top = $('.top');
        if (top.length) {
            height -= top.outerHeight(true);
        }
        $('.scroll-view').height(height + 'px');
    };
    adjustScrollHeight();
    $(window).on('resize', adjustScrollHeight);

    var interval = 250;
    var loading = function () {
        var wrapper = $('.loading');
        if (wrapper.length) {
            var dots = wrapper.find('[data-loading]');
            var count = dots.text().length;
            if (count >= 3) {
                count = 0;
            } else {
                count++;
            }
            dots.text('.'.repeat(count));
            setTimeout(loading, interval);
        }
    };
    setTimeout(loading, interval);

    var ble = function () {
        $('.scan-result').text('Scanning... This can take a while...');
        self.request('get', '/scan/trigger');
    };
    $(document).on('click', '.scan button', ble);
    if ($('.scan').length) {
        ble();
    }

    $(document).on('click', '.scan-result [data-address]', function (e) {
        e.preventDefault();
        var selection = $(this);
        var url = '/scan/select?ble_address=' + encodeURIComponent(selection.attr('data-address'));
        self.request('get', url, null, function () {
            window.location.href = '/';
        });
    });

    $(document).on('click', '[data-confirm]', function (e) {
        e.preventDefault();
        var control = $(this);
        if (confirm(control.attr('data-confirm'))) {
            window.location.href = control.attr('data-href');
        }
    });

    $(document).on('click', 'tr[data-toggle-checkbox]', function (e) {
        if ($(e.target).is('td')) {
            var control = $(this);
            var input = control.closest('tr').find('input[type="checkbox"]');
            input.prop('checked', !input.prop('checked'));
        }
    });

    self.bindProfilesMode();
};

App.prototype.update = function (data) {
    var self = this;
    var payload = JSON.parse(data);
    if (payload.hasOwnProperty('battery_info')) {
        payload = payload.battery_info;
        var table = $('.status-table');
        var nth = payload.slot + 2;
        table.find('[data-led] td:nth-child(' + nth + ') .led').attr('class', 'led ' + payload.led);
        table.find('[data-type] td:nth-child(' + nth + ')').text(payload.type);
        table.find('[data-mode] td:nth-child(' + nth + ')').text(payload.mode);
        table.find('[data-status] td:nth-child(' + nth + ')').text(payload.status);
        table.find('[data-voltage] td:nth-child(' + nth + ')').text(payload.voltage + ' V');
        table.find('[data-current] td:nth-child(' + nth + ')').text(payload.current + ' A');
        table.find('[data-capacity] td:nth-child(' + nth + ')').text(payload.capacity + ' mAh');
        table.find('[data-time] td:nth-child(' + nth + ')').text(payload.time);
        table.find('[data-temperature] td:nth-child(' + nth + ')').text(payload.temperature + ' °C');
        table.find('[data-resistance] td:nth-child(' + nth + ')').text(payload.resistance + ' mΩ');
        table.find('tr').each(function () {
            var row = $(this);
            row.find('td:nth-child(' + nth + ')').attr('class', payload.status.toLowerCase());
        });
    } else if (payload.hasOwnProperty('scan_results')) {
        $('.scan-result').html(payload.scan_results);
    }
};

App.prototype.bindProfilesMode = function () {
    var self = this;
    var wrapper = $('.wrapper.profiles-mode');
    if (!wrapper.length) {
        return;
    }

    $(document).on('input', '[data-profiles-form] input', function () {
        var control = $(this);
        var form = control.closest('form');
        form.find('[type="submit"]').attr('disabled', null);
    });

    $(document).on('click', '[data-profiles-form] [data-move]', function (e) {
        e.preventDefault();
        var control = $(this);
        var row = control.closest('tr');
        if (control.attr('data-move') === 'up') {
            row.prev().before(row);
        } else {
            row.next().after(row);
        }
        var form = control.closest('form');
        form.find('[type="submit"]').attr('disabled', null);
    });

    $(document).on('click', '[data-show-details]', function (e) {
        e.preventDefault();
        var control = $(this);
        var parent = control.closest('[data-id]');
        $('.modal[data-id="' + parent.attr('data-id') + '"]').modal('show');
    });

    $(document).on('click', '[data-set-slot]', function (e) {
        e.preventDefault();
        var control = $(this);
        self.request('get', control.attr('data-set-slot'), null, function (payload) {
            if (payload.hasOwnProperty('success')) {
                self.flashMessage(payload.success, 'success');
            } else {
                self.flashMessage(payload.error, 'danger');
            }
        })
    });
};

App.prototype.request = function (method, endpoint, data, callback) {
    var config = {
        method: method,
        headers: {
            'Authentication': window.token,
        },
        data: data,
        error: function (jqXHR, textStatus, errorThrown) {
            alert(textStatus + ': ' + errorThrown);
        }
    };

    if (callback) {
        config['success'] = callback;
    }

    $.ajax(endpoint, config);
};

App.prototype.flashMessage = function (message, type) {
    var wrapper = $('.flashes');
    var alert = $('<div class="alert"></div>');
    alert.addClass(type);
    var content = $('<div class="content"></div>');
    content.text(message);
    alert.append(content);
    var close = $('<button class="close">&times;</button>');
    close.on('click', function () {
        alert.remove();
    });
    setTimeout(function () {
        alert.remove();
    }, 5000)
    content.append(close);
    wrapper.append(alert);
};

$(function () {
    var app = new App();
    window.app = app;
    app.bind();
});
